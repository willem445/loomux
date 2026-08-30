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
//! this enum. Batch 4 turned both into free functions (`role_template`,
//! `role_instructions_file`) and left both in
//! `src-tauri/src/orchestration/mod.rs`, next to the `include_str!` templates
//! they load.
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
//! **Batch 5 split that pair, and the split is the sharper reading of the same
//! rule.** `role_template` stays: it loads the fixture-pinned bytes, so it is
//! the half the argument above is actually about. [`role_instructions_file`]
//! came across, because it loads nothing — it maps a class onto the file name
//! the *group directory* carries, and `workflow::Block::instructions_file`
//! calls it from inside this crate. Batch 4 kept them together on the ground
//! that "the name and the bytes are one mapping"; that pairing turned out to be
//! the weaker half of its own argument. What must not travel silently is
//! content and the procedure that blesses it, and a bare `"worker.md"` is
//! neither. The pin is unmoved either way: see this function's own doc comment
//! for the test that would redden.
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
//! violate by holding them. [`role_instructions_file`] joined them in batch 5
//! on the same terms and for the same reason — `pub` here so both `workflow`
//! and `mod.rs` can reach it.
//!
//! **What a `pub(crate) use` on the other side does and does not buy**, because
//! the convenient phrasing for this is wrong and was written twice before it
//! was caught. `orchestration` re-exports these `pub(crate)`, which governs the
//! FLAT spelling — `orchestration::default_model`,
//! `orchestration::role_instructions_file`, the ones every existing call site
//! uses — and nothing else. It does not narrow the item, because `mod.rs` also
//! re-exports this whole module publicly (`pub use loomux_engine::model::{self,
//! …}`), so every `pub` item here is reachable as `orchestration::model::…`
//! too. `role_instructions_file` was `pub(crate)` before batch 5 and is
//! publicly reachable by that path now. So the accurate claim is **"no existing
//! spelling widened"**, not "the public surface is unchanged".
//!
//! That reachability change is forced and harmless, which is why the answer is
//! to state it rather than to contort the re-export chasing a literal
//! "unchanged". Forced: the function has to be `pub` here for `src-tauri`'s
//! call sites to reach it at all, and `model::` was already a public re-export
//! before this batch. Harmless: `loomux-engine` is `publish = false` — an
//! internal workspace boundary, not a shipped library API — so "public" here
//! means reachable by a sibling crate in this repo, not a compatibility promise
//! to anyone outside it.

use serde::{Deserialize, Serialize};

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
/// scope (`mcp::tool_defs`) all key off this enum, and its values are a closed,
/// hand-written list — five a workflow file may name ([`workflow::kind_from_str`](crate::workflow::kind_from_str))
/// plus [`Role::Solo`], which no workflow can reach.
///
/// What each class *is* varies, and the enum should not be read as promising more
/// than it enforces — read [`Role::containment`] for the exact per-class tier. A
/// planner is structurally read-only ([`Role::is_read_only`] — editing tools AND
/// `git commit`/`git push` denied at the CLI). A reviewer (#462) is structurally
/// denied the CLI's *file-editing tools*, but keeps the shell, so its "never
/// pushes" stays instruction-backed; a manager (#1161) rides that same tier for
/// the same reason. The guarantee is over which posture a block gets, not that
/// every posture is a sandbox.
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
    /// **The human's own interface to the group** (#1161) — a pane the human
    /// converses with: project discussion, status, and the requirements
    /// engineering that turns a rough feature request into a groomed brief
    /// before the orchestrator ever sees it.
    ///
    /// A fifth capability class rather than a `role_hint` on a reviewer,
    /// because what distinguishes it is *structural* and a hint cannot express
    /// it: `doc/design/liaison.md` states its own promotion trip-wire — a third
    /// capability tool granted off "faces the human" — and this class fires it.
    ///
    /// Optional and **workflow-only**: a manager exists only when a repo's
    /// `.loomux/workflow.yml` declares `kind: manager`. The built-in roster is
    /// still exactly four blocks (`builtin_roster`), so a group with no workflow
    /// file never constructs one and behaves exactly as it did before this
    /// variant existed. `spawn_agent` refuses it for the same reason it refuses
    /// `orchestrator`: the manager is the human's fixture, not a delegate an
    /// orchestrator opens.
    Manager,
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
            // `mgr`, not `man`/`m`: the badge and roster conventions parse
            // `<prefix>-<seq>`, and a prefix that reads as a word is what makes
            // `mgr-3` legible next to `w-7`/`rev-5` in a task board row.
            Role::Manager => "mgr",
            // Solo panes mint their id as `solo-N` directly (see
            // `OrchRegistry::solo_prepare`), never through `block.prefix()` —
            // they have no block. Never reached in practice.
            Role::Solo => "solo",
        }
    }
    /// Lowercase wire/label name (matches the `Serialize` rename). Unlike
    /// `role_template` (which stayed in `src-tauri` with the templates it loads)
    /// and [`role_instructions_file`] (which came here in batch 5 — see this
    /// module's doc), this one IS reached for a solo member —
    /// `channel_member_label` formats it into the identity line every channel
    /// message/notice carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Orchestrator => "orchestrator",
            Role::Worker => "worker",
            Role::Reviewer => "reviewer",
            Role::Planner => "planner",
            Role::Manager => "manager",
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
            Role::Reviewer => Containment::NoEdits,
            // #1161. The reviewer's tier, chosen for the reviewer's reason: a
            // manager must READ the codebase to ground its questioning, and
            // must not write it. `ReadOnly` is the wrong rung and not merely a
            // stricter one — it forces unattended mode
            // (`Containment::forces_unattended`), which is hostile to a pane
            // whose entire purpose is a human sitting in front of it. As with a
            // reviewer, "the manager never pushes" therefore stays
            // instruction-backed; what is structural is the denied editing
            // tools (see `Containment::NoEdits` for the exact size of that).
            Role::Manager => Containment::NoEdits,
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

/// A per-CLI **ready marker** (#1591) — a shape the CLI's own output takes
/// once it is genuinely able to accept typed input, for a CLI whose painted
/// UI arrives well before that point.
///
/// This exists because the generic boot gate ([`CliCaps::ready_marker`]'s own
/// doc for the mechanism) is *painted and quiet*, and that pair is a proxy:
/// it asks whether the CLI has stopped writing, not whether it has started
/// reading. A TUI that draws its whole chrome, goes quiet, and only then
/// finishes connecting its MCP servers and its provider auth defeats the
/// proxy without doing anything wrong.
///
/// **Data, not a special case** (CLAUDE.md constraint 8), the same argument
/// [`CliCaps`] itself makes: "this vendor's TUI says it is up by printing X"
/// is a fact about the vendor, written down once in its row and consulted —
/// never an `if cli == "..."` at the gate.
///
/// One variant today, and it is a SHAPE rather than a literal on purpose: the
/// count in opencode's footer is the number of MCP servers that have
/// connected so far, so it moves while the handshake completes and can lag
/// what loomux configured. Matching the shape means the marker fires on the
/// first connected server rather than waiting for a number loomux would have
/// to keep in step with the CLI's own bookkeeping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyMarker {
    /// An ASCII digit immediately followed by this literal — a COUNT of
    /// something the CLI reports only once that thing is up.
    CountThen(&'static str),
}

impl ReadyMarker {
    /// Does `screen` — the pane's RENDERED rows, see the caller — show this
    /// marker?
    ///
    /// **Rendered, not the raw byte ring.** A status footer is the most
    /// cursor-positioned region of a TUI: a repaint may write the count and its
    /// label with separate absolute cursor moves, or wrap between them, so in
    /// the byte stream they need never be adjacent even though the human plainly
    /// sees `2 MCP`. Only a VT replay puts them in neighbouring CELLS. An
    /// ANSI-stripped ring read is the caller's FALLBACK when no trustworthy
    /// composition exists, not the primary reading.
    ///
    /// Two conditions, and each closes one direction of failure:
    ///
    /// - **A digit immediately before the literal.** Without it the bare label
    ///   releases the paste — a menu row, a `/mcp` help line, the word sitting
    ///   in a brief the pane is echoing back.
    /// - **The literal is not followed by a WORD** (#1591 review N3): after any
    ///   run of spaces, the next character must not be an ASCII letter. A count
    ///   is a LABEL — `2 MCP` at a row's end, or `2 MCP /status` — where a boot
    ///   line is a sentence (`1 MCP server connecting...`) and opencode's own
    ///   `/status` dialog is a caption over a DIFFERENT number
    ///   (`{...length} MCP Servers`, the configured count, not the connected
    ///   one). Without this, either satisfies the marker and the gate degrades
    ///   to exactly the pre-#1591 behaviour it exists to fix.
    ///
    /// Both conditions can only ever REFUSE, so a vendor whose footer this
    /// misreads waits out `READY_MAX_WAIT` — the direction the design note
    /// argues is safe — and never releases a paste early. The residual the
    /// label rule does NOT close (a label-shaped decoy elsewhere on the
    /// rendered screen) is stated in `doc/design/opencode.md`.
    ///
    /// Every occurrence of the literal is examined, not just the first: the
    /// footer this was written for carries other text on the same row, and a
    /// row that happens to contain the label twice must not be decided by
    /// whichever copy came first.
    pub fn matches(self, screen: &str) -> bool {
        let Self::CountThen(lit) = self;
        if lit.is_empty() {
            return false; // an empty literal matches everywhere, which is not a marker
        }
        let bytes = screen.as_bytes();
        let mut from = 0usize;
        while let Some(off) = screen[from..].find(lit) {
            let at = from + off;
            let counted = at > 0 && bytes[at - 1].is_ascii_digit();
            if counted && !followed_by_a_word(&screen[at + lit.len()..]) {
                return true;
            }
            // Resume past this occurrence. `at + lit.len()` is always a char
            // boundary (it is the end of a matched substring), where `at + 1`
            // would not be for a non-ASCII literal.
            from = at + lit.len();
        }
        false
    }
}

/// Does `rest` — whatever follows a matched marker literal — begin a WORD
/// rather than end a label? (#1591 review N3.)
///
/// Spaces are skipped first, so `2 MCP /status` and `2 MCP` at a row's end
/// both read as labels. A newline is not a letter, so the common case — the
/// count sitting at the end of its own rendered row — needs no special case.
///
/// **Either case**, which is the half that earns its keep. Lowercase alone
/// rejects the boot-line shape (`1 MCP server connecting...`) and accepts
/// opencode's own `/status` dialog, which renders `{...length} MCP Servers`
/// from the CONFIGURED count — a label-shaped string carrying a number that
/// means something else entirely, on a screen a human can summon at any time.
/// Rejecting any following letter covers both.
///
/// Deliberately ASCII-only and deliberately crude: this is a REFUSAL
/// heuristic, and every reading it gets wrong costs the ceiling's wait rather
/// than a released paste.
fn followed_by_a_word(rest: &str) -> bool {
    // #1591 RED EVIDENCE ONLY — the word rule removed. Never merged.
    let _ = rest;
    false
}

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
    /// A shape this CLI's output takes once it can actually accept typed
    /// input, required IN ADDITION to the generic painted-and-quiet boot gate
    /// before loomux pastes a kickoff into a freshly spawned pane (#1591).
    /// `None` — every row but opencode's — leaves that pane's gate exactly
    /// what it was.
    ///
    /// **Why a marker at all.** The generic gate waits for the pane to have
    /// painted (`READY_MIN_OUTPUT`) and then gone quiet (`READY_QUIET`),
    /// which is a proxy for "the CLI has attached its stdin reader". The
    /// proxy holds for a CLI that paints once it is up, and fails for one
    /// that paints its whole UI first and finishes coming up afterwards —
    /// the paste then lands in a startup buffer, and the human sees an idle
    /// agent with an empty box.
    ///
    /// **The bound, and which way it fails.** A marker only ever WITHHOLDS a
    /// paste, and only until `READY_MAX_WAIT`; the ceiling is untouched, so a
    /// CLI that changes its footer costs the ceiling's wait and is then pasted
    /// into exactly as it is today (audited as `TimedOut`, never silently).
    /// It cannot lose a delivery, and it cannot make one wait longer than the
    /// pre-#1591 worst case.
    pub ready_marker: Option<ReadyMarker>,
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
        // Claude Code's box is live from its first paint; the generic
        // painted-and-quiet gate has never mis-scored it (#1591).
        ready_marker: None,
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
        // Copilot's own delivery quirk is focus-gated KEYS, not a late input
        // loop — closed by the focus-in prefix on the submit bytes (#98), not
        // by waiting longer (#1591).
        ready_marker: None,
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
        // No mis-scored delivery observed on gemini, and no marker was
        // adopted speculatively: a row gets one when a pane on it is caught
        // painted-but-not-listening (#1591).
        ready_marker: None,
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
        // #1591: opencode's TUI paints its banner and input box (far past
        // `READY_MIN_OUTPUT`) and goes quiet while its MCP status is still
        // being fetched, so the generic gate calls it ready before it is
        // reading — observed on five deliveries in one session.
        //
        // The marker is the home footer's MCP segment, `⊙ 2 MCP /status`.
        // Read off the vendor's source rather than inferred from the
        // observation (`anomalyco/opencode`, tag v1.18.25 =
        // cb7d8b2f5e44876ef98b661dc10590c915af3a9f):
        // `packages/tui/src/feature-plugins/home/footer.tsx` renders
        // `{count()} MCP`, and `packages/tui/src/context/sync.tsx` initialises
        // `mcp: {}` and fills it from `sdk.client.mcp.status()` in the
        // NON-BLOCKING tail of `bootstrap()` — after `store.status` has left
        // `"loading"`, which is the same `ready()` the prompt box renders
        // under. So the segment cannot precede the input box. See
        // `doc/design/opencode.md`'s Readiness section for the premise, its
        // falsifier, and what a third-party footer plugin does to it.
        //
        // Matched as a SHAPE (a digit, then " MCP") rather than against a
        // number loomux could compute from its own config: the digit is the
        // CONNECTED count while the segment is gated on the CONFIGURED list
        // being non-empty, so `0 MCP` is a real and SETTLED rendering (the
        // `McpStatus` union has no "connecting" member) — and it is still
        // proof the handshake finished, which is the only thing this gate
        // needs to know.
        ready_marker: Some(ReadyMarker::CountThen(" MCP")),
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
        // codex is never spawned, so nothing ever waits on its boot.
        ready_marker: None,
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
/// planner, manager) get the strong tier and the executing ones (worker,
/// reviewer) the mid tier.
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
        // #1161: the manager joins the strong tier. Its output IS a
        // conversation with the human — eliciting requirements, spotting the
        // ambiguity nobody stated — so conversational quality is the product
        // here rather than a nicety. A block pins its own `model:` to disagree.
        Role::Orchestrator | Role::Planner | Role::Manager => "opus",
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

/// The file name a capability class's instructions are written under **in the
/// group dir**, and the half of batch 4's `Role::template()` split that came
/// across in batch 5. See this module's header for why the other half did not.
///
/// This one names no template and loads no bytes: it is the group directory's
/// own contract — the name a kickoff prompt tells an agent to read — and
/// `workflow::Block::instructions_file` is a caller of it that now lives in
/// this crate. The `include_str!` templates and the byte-golden fixture root
/// that blesses them stayed in `src-tauri` with `role_template`, which is the
/// thing batch 4's argument was actually protecting.
///
/// Its mapping is not a claim this crate makes on its own recognizance:
/// `the_toggle_off_leaves_every_instruction_file_byte_for_byte_what_it_was`
/// writes a default group's four instruction files and byte-compares them
/// against `src-tauri/tests/fixtures/pre222/`, which pins the name as well as
/// the bytes for each of those four. A mis-mapped name reddens there wherever
/// this function is defined.
///
/// [`Role::Manager`] (#1161) is deliberately outside that pin's reach, and the
/// asymmetry is the feature: no default group has a manager, so no default
/// group writes `manager.md` and there is nothing for a default-group pin to
/// compare. Its name is pinned instead by
/// `a_manager_block_writes_the_managers_own_instructions_file` (declaring one
/// and reading the group dir) and its bytes by the same live-vs-golden pairing
/// the other four get in `a_workflow_placeholder_must_sit_at_the_end_of_a_line_it_shares`.
pub fn role_instructions_file(role: Role) -> &'static str {
    match role {
        Role::Orchestrator => "orchestrator.md",
        Role::Worker => "worker.md",
        Role::Reviewer => "reviewer.md",
        Role::Planner => "planner.md",
        Role::Manager => "manager.md",
        Role::Solo => unreachable!("solo panes have no instructions file"),
    }
}

// #888 slice A3 batch 8 — four small pure items lifted out of
// `src-tauri/src/orchestration/mod.rs`, joining this module because each is
// data (a wire-form enum, two tunable defaults) rather than registry state.
// `mod.rs` re-exports all four under their original names, so every existing
// call site — in this crate and the integration suite — resolves unchanged.
// See doc/design/engine-extraction.md §6 for the batch record.

/// How a `deliver_prompt` call relates to the pane's lifecycle. Governs the boot
/// readiness wait AND the one-time copilot autopilot-consent confirm (#101).
///
/// **#620: also a PERSISTED fact** (`queue::QueuedDelivery::delivery_kind`).
/// A delivery held through a pause is drained minutes or hours later by
/// `flush_paused_queues`, long after the `deliver_prompt` call that knew what
/// kind it was returned — so the kind rides on the queue entry rather than
/// living only on the calling thread's stack. Serialized kebab-case
/// (`"fresh-kickoff"`) for the reason `QueuedPayload` spells its own tag out:
/// a human reading a group's `queue.json` after a crash should not need
/// serde's conventions to know what an entry is. `MidSession` is the `Default`
/// — see the field's doc for why that is the conservative reading of both an
/// older build's record and a restart.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Delivery {
    /// First prompt to a freshly *booted* pane (a fresh spawn's kickoff): wait
    /// for the CLI to paint, and — for an autopilot copilot agent — answer the
    /// "Enable autopilot mode" consent dialog before pasting.
    FreshKickoff,
    /// First prompt to a *resumed* pane: still wait for the CLI to paint AND
    /// (#364) still answer the autopilot consent dialog. Resume was assumed to
    /// restore allow-all/autopilot from the session event log so the dialog
    /// would never reappear — the human's #364 report is that this assumption
    /// is false (the dialog does reappear, or autopilot isn't restored), so
    /// this delivery confirms exactly like `FreshKickoff` does.
    ResumeKickoff,
    /// A mid-session delivery to an already-running pane (a follow-up / steer):
    /// no readiness wait, no dialog — long past boot, nothing to confirm.
    ///
    /// The `Default`, and deliberately the conservative one (#620): every
    /// treatment this enum can switch ON — a boot wait, a stray Enter into a
    /// consent dialog, arming #517's re-delivery — is an ACTION taken against
    /// a live pane, so a record that cannot say what it was must fall through
    /// to the kind that takes none of them.
    #[default]
    MidSession,
    /// The mandatory post-compact re-grounding notice, and **nothing else**
    /// (#1161 M2).
    ///
    /// Behaviourally this is a `MidSession` delivery in every existing respect
    /// — no readiness wait, no consent dialog, no lost-kickoff recovery — and
    /// `the_regrounding_delivery_answers_every_lifecycle_predicate_like_midsession`
    /// pins that, so the variant cannot quietly acquire a boot treatment by
    /// being added to one of the `matches!` lists above.
    ///
    /// It exists for exactly one reason: it is the **only** mid-session text
    /// loomux may type into a `Role::Manager` pane
    /// ([`Delivery::permitted_into_manager_pane`], decision D2 on #1161). That
    /// pane's transcript is a human's conversation, so the guarantee is that
    /// nothing is injected into it; the sole carve-out is this notice, because
    /// without it the directive-ledger survival mechanism — the one thing that
    /// carries the human's own instructions across a compact nobody saw coming
    /// — is dead for the one pane whose context IS the record of what the
    /// human said.
    ///
    /// **Why a variant and not a private back door.** A guard reads every one
    /// of its inputs by one rule; a carve-out spelled as a `Delivery` variant
    /// for the kickoff and as a bypassing function call for the re-grounding is
    /// two rules, and the second is invisible to any test that enumerates the
    /// first. Here the whole permitted set is a property of this enum, so one
    /// exhaustive set assertion covers it and a fourth carve-out cannot be
    /// added without failing that count.
    ///
    /// **Downgrade note:** this is a persisted kind
    /// (`queue::QueuedDelivery::delivery_kind`), so a `"regrounding"` entry
    /// held through a pause and then read by an older build fails that entry's
    /// parse. Downgrade safety was never on offer for this file — the same
    /// posture `humanq::OptionSpec` states — and the alternative, reusing
    /// `MidSession`, is the asymmetry this variant exists to avoid.
    Regrounding,
}

impl Delivery {
    /// **Whether loomux may type this into a [`Role::Manager`] pane** — the
    /// structural no-injection guarantee, as one pure predicate (#1161 M2).
    ///
    /// `true` for the two kickoffs and for [`Delivery::Regrounding`]; `false`
    /// for [`Delivery::MidSession`], which is what every other producer in the
    /// codebase sends: `channel_send`, `send_prompt`, watchdog and stall
    /// notices, `[loomux] answer to q-N`, lock grants, watch notices, the
    /// compact nudge. All of them funnel through `deliver_prompt`, so all of
    /// them are refused by this one answer rather than by N conventions at N
    /// call sites.
    ///
    /// The kickoff is permitted because it is delivered *before* the pane is a
    /// conversation — it is how every agent learns what it is — and the
    /// re-grounding for the reason its own variant doc gives.
    ///
    /// **This is a counterfactual, so it is pinned as one.** The permitted set
    /// is asserted as a SET over every variant
    /// (`exactly_three_delivery_kinds_may_enter_a_manager_pane`), not as three
    /// separate `assert!`s: a fourth carve-out folded in later fails the count,
    /// which is the failure a per-variant test would not produce.
    pub fn permitted_into_manager_pane(self) -> bool {
        matches!(
            self,
            Delivery::FreshKickoff | Delivery::ResumeKickoff | Delivery::Regrounding
        )
    }

    /// Every variant, for the set assertions that pin the predicates above.
    ///
    /// Hand-listed and therefore capable of going stale — so the one thing it
    /// must not do is go stale **silently**. [`Delivery::all_index`] below is a
    /// non-exhaustive-match tripwire that makes a fifth variant a compile
    /// error until this array grows with it, which is what lets a test read
    /// `ALL` and honestly claim to have covered every kind.
    pub const ALL: [Delivery; 4] = [
        Delivery::FreshKickoff,
        Delivery::ResumeKickoff,
        Delivery::MidSession,
        Delivery::Regrounding,
    ];

    /// This variant's position in [`Delivery::ALL`] — a compile-time
    /// completeness proof for that array, not a useful accessor.
    ///
    /// The `match` is exhaustive, so **adding a variant without an arm here
    /// does not compile**; adding the arm forces an index, and the only correct
    /// one is past the end of a four-element array, so `ALL` must grow too.
    /// `the_all_list_holds_every_delivery_kind_exactly_once` walks `ALL` and
    /// asserts each row reports its own position, which catches the remaining
    /// mistake — an arm given a duplicate or wrong index to make it compile.
    ///
    /// This is the convention CLAUDE.md states for a guard that must not be
    /// steppable: decide on an axis a rename or an addition cannot dodge (here,
    /// the compiler's own exhaustiveness), never on a name.
    pub const fn all_index(self) -> usize {
        match self {
            Delivery::FreshKickoff => 0,
            Delivery::ResumeKickoff => 1,
            Delivery::MidSession => 2,
            Delivery::Regrounding => 3,
        }
    }

    /// Whether to hold the paste until the CLI has painted its UI — true for
    /// either kickoff (the CLI has just been launched), false mid-session.
    ///
    /// `pub` here though it was bare module-private in `src-tauri` — batch 8's
    /// forced widening (same shape as this module's "Widened on the way in"
    /// section above): `mod.rs`'s own callers (`kind.wait_ready()`,
    /// `delivery.wait_ready()`) are on the other side of the crate boundary
    /// now, so nothing narrower than `pub` still reaches them.
    pub fn wait_ready(self) -> bool {
        matches!(self, Delivery::FreshKickoff | Delivery::ResumeKickoff)
    }
    /// Whether this delivery should watch for and answer copilot's "Enable
    /// autopilot mode" consent dialog (#364): true for EITHER kickoff — a
    /// fresh boot or a resume — since both can show the dialog; false
    /// mid-session, which is long past boot and has nothing to confirm.
    pub fn confirms_autopilot_dialog(self) -> bool {
        matches!(self, Delivery::FreshKickoff | Delivery::ResumeKickoff)
    }
    /// Whether a delivery of this kind may be RE-DELIVERED by the late
    /// monitor when it turns out never to have reached the pane (#517).
    ///
    /// `FreshKickoff` only — deliberately narrower than the two predicates
    /// above, which both include a resume. A fresh spawn's brief exists
    /// nowhere else: nothing will re-send it, and the agent has no other way
    /// to learn what it was spawned to do. A `ResumeKickoff` payload is a
    /// re-sync notice re-derived from durable state (see
    /// `resume_kickoff_notice`), and a `MidSession` prompt has a sender who
    /// is still around — both keep the pre-#517 badge-and-stop behavior.
    pub fn recovers_lost_kickoff(self) -> bool {
        matches!(self, Delivery::FreshKickoff)
    }
}

/// Autonomous mode (#83): default output-quiet window before an idle tick fires,
/// when the group's `idle_tick_minutes` guardrail isn't set. Lowered from the
/// original 15 to **5** after a live test: a human who turns autonomous mode on
/// expects action within a few minutes, and a 15-minute default simply never fired
/// in an 8-minute session. Per-group tunable (`set_idle_tick_minutes`) so the human
/// can drop it to 1–2 min to verify quickly. See `idle_tick_should_fire`.
///
/// `pub` here though it was bare module-private in `src-tauri` — batch 8's
/// forced widening, the same shape this module's "Widened on the way in"
/// section (above) argues for `default_model`/`sanitize_model_opt`. State the
/// reachability precisely rather than reach for "unchanged": `mod.rs`'s
/// `pub(crate) use` narrows only the FLAT spelling
/// (`orchestration::DEFAULT_IDLE_TICK_MINUTES`, the one `mod.rs` actually
/// calls) back to "this crate". It does NOT narrow the item overall —
/// `mod.rs` already re-exports the whole `model` module publicly (`pub use
/// loomux_engine::model::{self, …}`), so this const is also reachable as
/// `orchestration::model::DEFAULT_IDLE_TICK_MINUTES`, and since that path
/// crosses no crate-private boundary, as
/// `loomux_lib::orchestration::model::DEFAULT_IDLE_TICK_MINUTES` from outside
/// the crate too. Forced (an item must be `pub` here to cross the crate
/// boundary at all) and harmless (`loomux-engine` is `publish = false`; the
/// module was already a public re-export before this batch, and "public"
/// here means reachable by a sibling crate in this workspace, not a shipped
/// API promise).
pub const DEFAULT_IDLE_TICK_MINUTES: u32 = 5;

/// Idle-tick intake gate (#332/#429): the smart default `intake_poll_minutes`
/// resolves to whenever a group is autonomous and hasn't set an explicit
/// value — matches `DEFAULT_IDLE_TICK_MINUTES` (the idle tick's own
/// quiet-window default) rather than inventing a new cadence: the poller
/// need not run more often than the tick it feeds ever fires anyway.
///
/// Unlike [`DEFAULT_IDLE_TICK_MINUTES`] above, this one has **no flat
/// `orchestration::` re-export left**, and the reason is worth stating rather
/// than leaving as an asymmetry the next reader has to re-derive: batch 8 added
/// one because `intake.rs` was the sole caller and was still in `src-tauri`.
/// Batch 11 moved [`crate::intake`] into this crate, so its call is
/// `crate::model::DEFAULT_INTAKE_POLL_MINUTES` and nothing in `src-tauri`
/// spells the const in code at all. The re-export list in
/// `orchestration/mod.rs` is meant to be readable as the live list, so a line
/// with no consumer left comes off it. The item stays `pub` here — forced, and
/// reachable as `orchestration::model::DEFAULT_INTAKE_POLL_MINUTES` through
/// that file's module re-export, on the same harmless terms.
pub const DEFAULT_INTAKE_POLL_MINUTES: u32 = DEFAULT_IDLE_TICK_MINUTES;

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire/label name of every capability class, written out once as
    /// literals rather than derived from either producer.
    ///
    /// Deriving the expectation from `as_str()` would pin `Serialize` to
    /// whatever `as_str` happens to say, and vice versa — the table has to be a
    /// third party or neither test means anything. These six strings are a
    /// **persisted and cross-process contract**: they are what `agents.json`
    /// carries between app launches, what `list_agents`/`session_roles` hand
    /// the webview, and what the frontend matches on to decide a roster row's
    /// badge. Changing one is a breaking change to a state file, not a rename.
    const WIRE_NAMES: [(Role, &str); 6] = [
        (Role::Orchestrator, "orchestrator"),
        (Role::Worker, "worker"),
        (Role::Reviewer, "reviewer"),
        (Role::Planner, "planner"),
        (Role::Manager, "manager"),
        (Role::Solo, "solo"),
    ];

    /// `Role`'s serde form is `rename_all = "lowercase"`, and this states it
    /// per variant in one place instead of leaving it to whichever behavioural
    /// test happens to round-trip a class.
    ///
    /// Be precise about how much of this the integration suite already had,
    /// because most of it did. `list_agents` assertions pin `worker`,
    /// `reviewer` and `planner`, and `sessions_backfill_from_audit_when_roster_
    /// predates_it` pins `orchestrator` — it deletes `agents.json` and reads the
    /// class back out of the **audit log**, which is written through this same
    /// `Serialize`. A planted `#[serde(rename)]` on `Orchestrator` reddens that
    /// test, and it is not this one's to claim.
    ///
    /// `Solo` is the variant nothing reached, and the one this test is really
    /// for. It is also the variant whose wire name is least likely to be
    /// noticed: a solo pane is a `__solo__` pseudo-group member that never
    /// traverses a spawn, so a rename on it would change what every future
    /// `agents.json` records while every behavioural test stayed green.
    ///
    /// `manager` (#1161) is here for a third reason again: it is also the
    /// string a repo's `.loomux/workflow.yml` writes as `kind:`
    /// (`workflow::kind_from_str`) and the one `src/orchbadge.ts` matches to
    /// label the pane `MGR`. Renaming its serde form would break a repo file
    /// and a badge, not just a state file.
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

    // No containment-tier table test here, deliberately. One was written and
    // then removed: `src-tauri/tests/orchestration.rs`'s
    // `every_capability_class_pins_its_deny_tier` already asserts exactly that
    // mapping, and a planted `Role::Reviewer => Containment::None` reddens it
    // plus five spawn-path tests. A second copy in this crate would have been a
    // duplicated mechanism whose only red is one another test already produces
    // — and it can never be the FIRST thing to fail, since cargo runs the
    // `src-tauri` targets before this crate's and stops at the first failing
    // one.
}

#[cfg(test)]
mod delivery_tests {
    use super::*;

    #[test]
    fn the_all_list_holds_every_delivery_kind_exactly_once() {
        // `all_index`'s match is exhaustive, so a FIFTH variant is a compile
        // error until it is given an arm — and the only index left is past the
        // end of this array, so the array must grow with it. That is what makes
        // "every kind" below an honest claim rather than a hopeful one.
        for (i, d) in Delivery::ALL.iter().enumerate() {
            assert_eq!(d.all_index(), i, "{d:?} is not where ALL says it is");
        }
        assert_eq!(Delivery::ALL.len(), 4);
    }

    #[test]
    fn exactly_three_delivery_kinds_may_enter_a_manager_pane() {
        // A SET assertion, deliberately, not three `assert!`s (#1161 M2): the
        // carve-outs in the no-injection guarantee are a counterfactual — a
        // later slice folding a fourth kind in "just for this one notice" is
        // exactly the edit this pin exists to catch, and only a count catches
        // it.
        let permitted: Vec<Delivery> =
            Delivery::ALL.into_iter().filter(|d| d.permitted_into_manager_pane()).collect();
        assert_eq!(
            permitted,
            vec![Delivery::FreshKickoff, Delivery::ResumeKickoff, Delivery::Regrounding],
            "the manager pane's permitted deliveries are the two kickoffs and the post-compact \
             re-grounding notice, and nothing else — see doc/design/manager.md"
        );
        // The negative control, named rather than implied: MidSession is what
        // channel_send, send_prompt, every watchdog/stall notice, the answer
        // notice and the lock/watch notices all send.
        assert!(
            !Delivery::MidSession.permitted_into_manager_pane(),
            "a mid-session delivery is the whole class this guarantee refuses"
        );
    }

    #[test]
    fn the_regrounding_delivery_answers_every_lifecycle_predicate_like_midsession() {
        // The variant exists to be distinguishable AT THE MANAGER GATE and
        // nowhere else. If it ever picks up a boot treatment — a readiness
        // wait, the copilot consent Enter, #517 re-delivery — that is a change
        // to how a compacted pane is re-grounded in EVERY group, which is not
        // what #1161 asked for and would arrive silently by someone adding it
        // to one of the `matches!` lists.
        for probe in [
            (Delivery::wait_ready as fn(Delivery) -> bool, "wait_ready"),
            (Delivery::confirms_autopilot_dialog, "confirms_autopilot_dialog"),
            (Delivery::recovers_lost_kickoff, "recovers_lost_kickoff"),
        ] {
            let (f, name) = probe;
            assert_eq!(
                f(Delivery::Regrounding),
                f(Delivery::MidSession),
                "{name} must treat a re-grounding exactly as it treats a mid-session delivery"
            );
        }
    }

    #[test]
    fn the_regrounding_kind_round_trips_its_persisted_wire_name() {
        // It rides `queue::QueuedDelivery::delivery_kind` into queue.json, so
        // the kebab-case name is a persisted contract from the first write.
        let json = serde_json::to_string(&Delivery::Regrounding).unwrap();
        assert_eq!(json, "\"regrounding\"");
        assert_eq!(
            serde_json::from_str::<Delivery>("\"regrounding\"").unwrap(),
            Delivery::Regrounding
        );
    }
}
