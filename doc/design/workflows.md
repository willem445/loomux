# User-defined agent workflows: the block model

Issue #222. This note covers the backend core — roles as data,
`<repo>/.loomux/workflow.yml`, and compiling a block's persona down to each
agent CLI's native custom-agent flag (**sub-PR 1**) — and, at the end, **the
switch that turns any of it on**: the launcher's *advanced orchestrator* toggle
and the workflow-aware role templates (**sub-PR 4**). The workflow pane is
sub-PR 2; the `review_verdict` tool and gate enforcement are sub-PR 3.

## The problem

Before this change, an agent's identity *was* its `Role` — a closed four-variant
enum that decided, all at once, the persona, the instruction template, the
model, the CLI and the capabilities. Around 72 `Role::` match sites in
`orchestration/mod.rs` fanned out from it.

That made a perfectly reasonable request impossible to express: *"three
reviewers — one for security, one for perf, one for test quality — each with its
own focus prompt and its own model."* You could already **spawn** three
reviewers (nothing caps the count; the sequential worker→reviewer pipeline is
prose in `templates/orchestrator.md`, not code). What you could not do was
**declare** them.

## The split: identity is data, capability is an enum

```
      before                              after
   ┌──────────┐                   ┌─────────────┐   ┌──────────────┐
   │   Role   │  = everything     │   BlockId   │   │     Role     │
   └──────────┘                   │ (identity)  │   │ (capability) │
                                  │  unbounded  │   │    CLOSED    │
   persona ─┐                     └─────────────┘   └──────────────┘
   template ├─ all from                  │                  │
   model    │  one enum          persona, cli, model   deny-flags, cwd rule,
   cli      │                    prompt / profile      MCP tool scope
   caps ────┘
```

- A **`BlockId`** (a string — `rev-security`) is the agent's identity. Edges,
  gates, `spawn_agent(block:)` and the roster all reference it.
- **`Role` survives as the block's `kind`** — its *capability class*. It is
  still a closed enum with exactly four values, and every structural guarantee
  keys off it: the CLI-level deny flags in `build_agent_command`, the
  cwd/worktree rule in `spawn_agent_ex`, the MCP tool scope in `mcp::tool_defs`.
- Persona, CLI and model are unbounded data on the block.

`prefix()`, `template()` and `instructions_file()` moved off `Role` onto
`Block`; `Guardrails`' eight flat per-role fields (`worker_cli`,
`reviewer_model`, …) became one `blocks: Vec<Block>`; `cli_for(role)` /
`model_for(role)` became lookups into it (returning the *default block for that
class*, which for the built-in roster is the only one).

**The honest summary of "custom roles":** you can have five reviewers with five
prompts and five models — but all five are *reviewers* in the capability sense,
and a repo file cannot make any of them anything else. That is the feature, not a
limitation.

And "in the capability sense" is worth pinning down, because the enum enforces
less than the phrase suggests. A **planner** is structurally read-only: its
file-editing tools and `git commit`/`git push` are denied at the CLI level, so
`is_read_only()` is a mechanical guarantee. A **reviewer**'s "never pushes" is
*instruction-backed* — it holds the same write surface a worker does and is
merely told not to use it, exactly as before #222. What the closed enum
guarantees is that **a repo file cannot change which posture a block gets**; it
does not claim every non-worker posture is a sandbox. `doc/design/orchestration.md`
draws the same structural-vs-instruction-backed line, and this feature inherits
it rather than tightening it.

## Capability closure — the security argument

A workflow file is repo-authored input. Anyone who can open a PR against a repo
can propose one, and under `auto_ops` nobody approves the resulting agents' tool
calls. So the rule is absolute:

> **A workflow file can never grant a capability.**

Mechanically, that holds because everything a block can influence is either
inert text or a choice from a value set loomux already ships:

| Block field | What it can do | Why it's safe |
|---|---|---|
| `kind` | select one of 4 classes | closed enum; unknown values are **rejected**, not coerced (see below) |
| `cli` | select `claude` \| `copilot` | validated against `SUPPORTED_CLIS` at parse *and* at spawn |
| `model` | name a model | `sanitize_model` — the pre-existing allowlist filter |
| `prompt` | free text | inert; sanitized, then delivered as a persona **addendum**, never as a replacement for the loomux contract |
| `profile` | name a repo file | confined to the repo (no `..`, no absolute path, no drive prefix) |
| `allow` | add tool patterns | **banned outright on a read-only class** (see below); inert for the classes that already hold the write surface |
| `id` | name the block | reserved: the four class names may only be used by their own class, so no block can hijack another's contract file |
| *(on a `kind: orchestrator` block)* | pin `cli` / `model` only | `prompt:`/`profile:`/`allow:` are a **parse error** — the trust root is not a repo-writable surface (see below) |
| — | grant write access | **no spelling exists.** No `read_only:`, no fifth class, no capability key of any kind |

`deny_unknown_fields` on the wire types is what makes that last row true: a
`read_only: false` in a block isn't ignored, it's a validation error.

### Unknown `kind` is rejected, never coerced

Pre-#222, *two* places parsed a kind string as `_ => Role::Worker` —
`mcp.rs:320` (the `spawn_agent` tool) and `mod.rs:8366` (session rejoin). A
typo'd, hallucinated or corrupt kind therefore produced **a worker**: a
dedicated git worktree, write access, and PR authority, handed out on a guess.

Both are gone. An unrecognized kind is now a named error that lists the four
classes that *are* allowed.

That fix has a sharp edge, and a review caught it: the old catch-all was also,
accidentally, what stopped `kind: "orchestrator"` from resolving. Making unknown
kinds an *error* let a real one through — and an orchestrator-kind spawn skips
the live-agent cap and the spawn-rate backstop (both live inside `if role !=
Role::Orchestrator`) and passes `require_orchestrator`, so it holds the
privileged tool set. An orchestrator calling `spawn_agent(kind: "orchestrator")`
in a loop would fork-bomb the machine with fully-privileged panes. The MCP tool
now refuses that kind explicitly — the JSON-schema `enum` in `tool_defs` is
advertisement and is never checked against incoming arguments, so the check has
to be in `call_tool`. `mcp_spawn_refuses_kind_orchestrator` pins it.

### The orchestrator block is loomux-owned

A repo may pin the orchestrator's `cli` and `model`. It may **not** author its
`prompt:`/`profile:`, and may not give it `allow:`. Both are parse errors, and
both are dropped-and-audited if they arrive from a hand-edited `group.json` that
never met the parser.

This one is not a capability argument, and it is worth being clear about that:
the orchestrator already holds every tool, so a repo-authored prompt grants it
nothing *new*, and a malicious repo under `auto_ops` can already reach code
execution through a worker. It is a **trust** argument. The orchestrator is the
group's trust root — it runs unsupervised under `auto_ops`, in the repo root with
no worktree, holding the privileged MCP surface (`spawn_agent`, `kill_agent`,
`set_state`). Letting a file that arrives with a `git clone` write its system
prompt is a direct prompt-injection seam into that root (the #189 class), and it
would have been the one orchestrator path with no gate.

The asymmetry is what makes it indefensible rather than merely unfortunate: this
feature spends real effort making a *second* orchestrator impossible
(`spawn_agent(kind: "orchestrator")` refused at the MCP tool, an orchestrator
block refused at `spawn_agent_ex`) — and leaving the *first* one's persona
repo-writable would make that effort decorative. The stated feature ("five
reviewers, five prompts") needs none of it. If app-level orchestrator
customization is ever wanted it can arrive as an explicit human opt-in, which is
a categorically different thing from a file you get by cloning a repo.

The enforcement sits in `resolve_persona` rather than only in `persona_inject`,
because that is the single point both the CLI flags *and* the block's instruction
file resolve through — so a `mode: replace` orchestrator persona cannot rewrite
`orchestrator.md` either.

### `allow:` is banned on a read-only class

The other edge the same review found. A planner is read-only by **denying a fixed
list** — Edit, Write, MultiEdit, NotebookEdit, `git commit`, `git push`. Deny
beats allow on both CLIs, so an allow pattern cannot re-grant anything *on that
list*. But it doesn't have to: `allow: Bash(python *)` is named nowhere in the
deny list, and under `auto_ops` nobody approves the call — so the planner gets a
pre-approved shell that writes files, and the closure claim above becomes false.

Nobody can enumerate every write-capable program, so the rule runs the other way
round: **a read-only block gets no allow patterns, from any source.** The parser
rejects `allow:` on a read-only block (and says why); `persona_inject` drops any
that arrive from a `.github/agents` persona's frontmatter or a hand-edited
`group.json`, and audits the drop. For worker and reviewer — classes that already
hold the write/shell surface structurally — `allow:` widens nothing, and is just
an approval prompt the author has chosen to skip.

### Sanitization

Block ids reach a `--agent` flag and a file name; display names reach a pane
title; persona bodies reach a shell token. All three are filtered before they
get there, following the `sanitize_model` precedent — **strip, don't escape.**

The persona case is the subtle one. The `claude --agents '<json>'` payload is
the only place loomux puts free text on a command line. It rides inside **single
quotes**, and in both PowerShell and POSIX `sh` a single-quoted string is fully
literal *except for the quote character itself*. So `'` is the only character
that could break out — and `sanitize_persona` maps it to `’` (U+2019), which
keeps the prose readable ("don't" still reads as "don't") while making the
payload structurally inert. The JSON is then ASCII-escaped, so the command line
survives a pane whose code page isn't UTF-8. Escaping per-shell was rejected:
the same string is used as a PowerShell line *and* a POSIX line, and no single
escaping is correct for both.

## The schema

`<repo>/.loomux/workflow.yml` — committed and shareable, because a project's
workflow belongs to the project (the #51 requirement), and because every
coding-agent tool surveyed keeps its config as text in the repo.

```yaml
version: 1
name: focused-review
authored_with: loomux 0.8.0   # optional stamp; the workflow pane writes it.
                              # NEVER a validation error, whatever it says.

blocks:
  - id: planner              # IMMUTABLE identity. Edges/gates reference THIS.
    name: Planner            # display only; renaming never breaks a reference
    kind: planner            # capability class (closed enum)
    cli: claude
    model: opus

  - id: worker
    kind: worker
    cli: copilot
    profile: .github/agents/worker.md   # -> copilot --agent worker  (NATIVE)

  - id: rev-security
    name: Security review
    kind: reviewer
    cli: claude
    model: opus
    prompt: |                # -> claude --agents '{...}' --agent rev-security
      Review ONLY for security defects: injection, authz, secrets.
      Ignore style and perf — other reviewers cover those.

edges:                       # ADVISORY — the declared happy path
  - { from: planner, to: worker }
  - { from: worker,  to: [rev-security, rev-tests] }

gates:                       # DECLARED here; ENFORCED in the gh shim (sub-PR 3)
  merge:
    require: all-pass        # or: threshold: 2
    reviewers: [rev-security, rev-tests]
    also: [ci-green]
```

Design commitments, each earned from a specific failure in another tool:

- **`id` is immutable and human-meaningful; `name` is display-only.** n8n keys
  its graph by *display name*, so renaming a node silently breaks every
  expression referencing it — a bug class its own maintainer calls "far from
  perfect". Dify uses millisecond timestamps as ids.
- **No coordinates in the semantic file.** Layout goes in
  `.loomux/workflow.layout.json` (sub-PR 2's concern). Dify, ComfyUI and
  Langflow all embed x/y, so a canvas nudge churns the logic diff.
- **A real pre-run validation pass**, reporting *every* problem rather than the
  first: unknown kind, unknown CLI, an edge to a nonexistent block, a gate
  naming a nonexistent reviewer (or naming a *worker*, which would be
  permanently unsatisfiable), a threshold no number of passes could reach,
  duplicate ids, and unknown keys. This is the thing every surveyed tool
  skipped — Flowise, Langflow and Dify all discover these at runtime, and Dify
  will happily *publish* a workflow whose plugin node isn't installed.
- **A broken file is audited and skipped, never fatal.** The group falls back to
  the built-in roster and every agent still spawns. A repo file must not be able
  to stop a group from launching.
- **Quoted scalars keep their contents.** `allow: ["Bash(gh pr view --json
  title,body)"]` is a real tool pattern, and both the parse (a comma inside a
  quoted scalar is *content*, not a separator) and the sanitizer (which keeps
  commas) have to leave it intact. A filter that dropped the comma would not
  reject the pattern — it would silently rewrite it to `--json titlebody`, a
  different and broken command the agent is then pre-approved to run. Coordinated
  with #223, which hit the parse half of this.

### Why edges are advisory

The issue's framing — "define the flow through agent blocks" — implies a graph
the runtime walks. We deliberately don't build one. The orchestrator's
scheduling judgment *is the feature*: it decides whether a change is sprawling
enough to serialize or independent enough to parallelize across worktrees, when
to plan first versus go straight to a worker, when to reuse an idle delegate.
That is `doc/design/orchestration.md`'s Principle 3 — *guardrails in the
platform, judgment in the prompt*. A static DAG would replace those calls with
conditionals, which is exactly the 500-line-YAML sprawl GitHub Actions users
hate. (LangChain declined to build a visual workflow builder for the same
reason; OpenAI shipped Agent Builder and is deprecating it, with the migration
path being *back to code*.)

So: **declare the roster and the gates; let the orchestrator route.** The file
says which blocks exist, what each is for, and what must be true before a merge.
The orchestrator decides *when*. Its kickoff prompt lists the declared blocks
and says in as many words that the edges are advisory.

## Personas: compiled to native flags

Both agent CLIs now ship a custom-agent flag, and they are asymmetric in a way
that decides the whole design (verified against the installed CLIs' `--help`):

- `claude --agent <name>` **and `claude --agents '<json>'`** — a persona can be
  defined **inline**, with no file anywhere.
- `copilot --agent <name>` — resolves a *name* against `.github/agents/`. There
  is **no inline form**.

So loomux compiles a block's persona into whatever that CLI can consume:

| block persona | claude | copilot |
|---|---|---|
| none | nothing — the pre-#222 command, byte for byte | nothing |
| `prompt:` (inline) | `--agents '<json>' --agent <id>` | **kickoff-prompt injection** |
| `profile: .github/agents/x.md` | file body → `--agents` + `--agent` | `--agent x` (native) |

The empty cell that isn't there: **loomux never writes a generated persona into
the user's `.github/agents/`** to make Copilot's `--agent` reachable. That would
dirty their git tree with files they didn't author. A Copilot block with an
inline `prompt:` gets the persona as kickoff text instead — every CLI reads its
first prompt.

One subtlety in the Copilot column, worth stating because it is invisible until
it bites: `--agent` takes a **name**, and a persona's name comes from its
frontmatter, not from its path. So `.github/agents/security-review.md` can
perfectly well declare `name: worker` — and loomux would kind-check the
security-review file while Copilot went off and loaded the *worker* persona, with
the audit line insisting all was well. So the native path is taken only when the
handle resolves back to the file the block pointed at, unambiguously
(`profiles::handle_resolves_to`). If it doesn't — a name collision, or a name
that names something else — loomux falls back to kickoff injection, which
delivers the persona it actually read, and audits why.

A kickoff-delivered persona is framed as an **addendum**: it is introduced as a
persona layered on the loomux mechanics in the instructions file that the same
prompt points at. Repo text never gets to read as "ignore your instructions".

## Harvested from PR #105

PR #105 (`agent-prototype`, superseded) built roughly 70% of this backend
against the older `--append-system-prompt-file` design. `profiles.rs` came over
close to wholesale: `AgentProfile`, `discover_profiles`, `parse_profile` (the
lenient frontmatter skim that digests real Copilot agent files — folded
descriptions, `---` separators inside the body, copilot-native keys loomux
doesn't own), the `allow:` sanitizer, and `ProfileMode::{Append, Replace}` with
its **non-overridable `mechanics_core`**. Credit is in the commit message.

Two things changed in the move to the block model:

1. **A persona no longer maps *itself* onto a role.** #105 auto-applied a
   `.github/agents/worker.md` to the worker role by filename. Now the workflow
   file says which block uses which persona, so a persona file cannot take
   effect just by existing — it is opt-in, by reference. The `kind:`
   frontmatter survives only as a **compatibility check**: a persona that
   declares `kind: worker` while the block using it is a `planner` is an
   *error*, not a quiet promotion out of the read-only class.
2. **Claude gets the native flag**, not an appended system-prompt file. The
   `--agents` flag post-dates #105's design and is strictly simpler.

`trust_repo_mcp` stays **default-off** with a per-repo human opt-in — a repo
`.mcp.json` `stdio` entry is an arbitrary command loomux would launch, i.e.
local code execution with no per-call approval under `auto_ops`.

### Append vs replace, and what a persona can never take away

- **`append`** (the default, and the only mode an inline `prompt:` can be):
  loomux's built-in role contract still applies; the persona layers on top.
- **`replace`** (a persona *file* only — replacing loomux's role body is a
  deliberate, reviewable act): the persona replaces the role body, but loomux
  writes `mechanics_core(kind)` into the block's instruction file regardless.

The mechanics core is the functional contract that makes the app work: the MCP
tools, `report(status, summary)` discipline, the task board, the branch→PR git
flow, and *never merge — the human gates merges*. A replace persona can change
who the agent is. It can never leave it unable to report, or able to merge.
`replace_mode_persona_still_gets_the_mechanics_core` pins that.

## Nothing changes when there's no workflow in play

The compatibility guarantee, and the thing most of the test suite defends: a
group with no workflow in play — the advanced-orchestrator toggle off (the
default; see below), or on in a repo with no `.loomux/workflow.yml` — gets a
synthesized roster of exactly today's four blocks — ids `orchestrator` / `worker` / `reviewer` / `planner`, no
personas, inheriting the launcher's per-role CLI and model picks. Because the
ids are the role names, the instruction files keep their historic paths
(`worker.md`), the agent ids keep their historic prefixes (`w-3`), and because
no block has a persona, `PersonaInject::default()` adds no flag at all.

`default_roster_command_lines_match_legacy` asserts the emitted command lines
against strings copied verbatim from the pre-existing snapshot test. The kickoff
text is unchanged too — the roster paragraph is empty unless a workflow file
declared something.

Some seams worth knowing, most of them found by an adversarial review of the
first draft rather than by design:

- **The orchestrator block is always guaranteed.** A workflow that declares only
  the agents it cares about (three reviewers, a worker) still gets an
  orchestrator block synthesized — a group structurally cannot run without one.
  It is the only block loomux adds on the repo's behalf.
- **A class the file didn't declare has no block.** `spawn_agent(kind: planner)`
  against a roster with no planner says so plainly rather than guessing. The one
  place that would have been a silent failure is the launcher's *initial workers*
  count: a review-only workflow has no worker block, so those spawns would each
  have failed and the human would have gotten zero panes with only an audit line
  to explain it. The orchestrator is now told, in its pane.
- **The four class names are reserved ids.** `- id: planner, kind: reviewer` is a
  validation error, because a block's contract file is `<id>.md` and that block
  would write `reviewer.md` — the real reviewer's file. (It also breaks the
  orchestrator synthesis above, by letting a non-orchestrator hold the id
  `orchestrator`.) `clamped()` re-enforces the rule, plus id uniqueness, for
  rosters that arrive from a hand-edited `group.json` and never meet the parser.
- **A stale block id degrades, it doesn't fail.** A session recorded against
  `rev-security` still rejoins after the workflow file renames that block — as
  the class default, audited. Losing the persona is a downgrade; losing the
  session is data loss, and the human has no other way to reach it.
  `spawn_agent(block:)` stays strict, because there a typo *should* be an error.

## Persistence

`group.json`'s `guardrails` gained a `blocks` array and lost the eight flat
per-role fields. The **reader still understands the old shape**: a group.json
written by 0.8.0 is migrated on read into the equivalent four blocks, so a group
launched before this change rejoins with exactly the CLIs and models it had.
Nothing writes the flat fields again.

`AgentEntry` and the durable `AgentRecord` both gained `block`, so a resumed
`rev-security` session comes back as *that reviewer*, with its persona — not as
a generic one. The field is `#[serde(default)]`; a roster row from before blocks
falls back to the class's default block.

The spawn audit records the block and how its persona reached the CLI
(`copilot --agent` / `claude --agents` / `kickoff`), so a run stays reproducible
after the workflow file changes.

## The advanced-orchestrator toggle (sub-PR 4)

Everything above describes what a workflow file *does*. This section is about
when it is allowed to do it.

`Guardrails::advanced_orchestrator` is a per-launch boolean, default **off**.
Off, `create_group` does not open `.loomux/workflow.yml` — not "opens it and
ignores it", **does not open it**. There is no code path from the file to the
group, which is the cheapest possible way to keep the compatibility promise: the
default experience cannot regress on a file it never reads. On, the load-and-
validate above runs and the file's blocks become the roster.

### Why it isn't just "a file that exists takes effect"

That was the shape until this sub-PR, and it is wrong for one reason: **a
workflow file arrives with a `git clone`.** Anyone who can open a PR against a
repo can propose one. Without a toggle, cloning a repo and launching an
orchestrator would silently run *that repo's* agents, with *that repo's*
personas, before the human had ever seen the file.

The capability closure (above) means the worst case is bounded — a repo file can
never grant a capability, so those agents can't do anything loomux's own agents
couldn't. But "bounded" is not "consented to". The persona of every delegate is a
thing the human should have looked at, and the toggle is what makes them look:
tick it and the launcher shows the resolved roster — every block, its kind, CLI
and model, and **which blocks carry repo-authored personas** — before the group
spawns.

The toggle is persisted in `group.json` (absent → `false`, so every group launched
before this field rejoins as what it was: a built-in roster). A resumed
orchestration rebuilds its guardrails from that file, not from a launcher form.

### A resumed group runs the roster it was launched with

The consent above has a corollary that took a review round to see clearly
(rev-11 F2). If the launcher preview is *the* consent moment, then nothing that
happens afterwards may quietly change what the human agreed to — and a resume is
not a consent moment, because nobody is being shown anything.

So `create_group` takes a `Launch` (`Fresh` | `Resume`), and **only a fresh launch
reads `.loomux/workflow.yml`.** A resume runs the blocks persisted in `group.json`
— the ones the human actually looked at. Without that, the sequence

> launch with the advanced orchestrator on, having reviewed the roster → `git pull`
> (or check out a contributor's branch), which adds a reviewer block with a persona
> → close the orchestrator and reopen it from the session browser

hands the resumed group a delegate, and a repo-authored persona, that its human
never approved and was never shown. The blast radius is bounded by the capability
closure, as ever; the *consent* is not bounded by anything, which is the whole
point of having a toggle.

Drift is **audited, never applied**: on a resume whose roster no longer matches
what the file now resolves to, loomux writes `workflow-changed-since-launch` with
both block lists. A silent pin would be indistinguishable from a stale read. To run
a changed workflow you launch a group — which shows you the new roster first.

Note that `Launch` is deliberately *not* "does `group.json` already exist". A human
who edits their workflow and launches again on the same repo **is** at the launcher,
has seen the new preview, and must get the new roster; keying the pin off
group-exists would make editing your workflow file appear to do nothing, forever —
a worse bug than the one being fixed. `relaunching_after_editing_the_workflow_picks_up_the_new_file`
pins that half.

### Going live: the toggle mid-session (#316)

Everything above was launch-time-only: flipping the toggle meant ending the
group and relaunching, which throws away the orchestrator's context along with
its pane. #316 makes the toggle a **live** control — a groupview button, not
just a launcher checkbox — and the case for it is the same consent story as
the toggle itself: a human who is *already looking at the roster* (the
groupview chrome the toggle now shows, per **Surfacing the workflow**, below)
can consent to a change the same way the launcher preview does, without
needing to tear the group down first.

The live setter is not a new mechanism — it is modeled on the two setters that
already do exactly this shape: `set_max_agents` (validate → persist-first via
an in-place `group.json` patch → update the in-memory guardrail → audit →
deliver a `[loomux] …` notice) and `set_autonomous` (same shape, also
notice-delivering). Turning the toggle **on** re-runs the identical
`load_workflow` → `sync_merge_gate` → `Guardrails::clamped()` sequence a fresh
launch already runs — not a second loader — and swaps `guardrails.blocks` for
*future* spawns only. Turning it **off** clears `merge_gate` and rebuilds the
built-in four-block roster from the group's default CLI and per-role model
picks (`workflow::default_roster`), which is also not new work: it's the same
converter the launcher already uses.

**Why a live delegate's persona never changes underneath it.** *A resumed
group runs the roster it was launched with*, above, forbids retroactively
re-personaing a running session because a resume is not a consent moment.
Flipping the toggle live **is** one — the human clicks it while seeing the
roster — but that only licenses a decision about the *future*: new spawns use
the new roster; an agent already spawned keeps the block identity — and
persona — it was spawned under. Swapping a live delegate's identity out from
under a conversation it's mid-turn on is a different, larger claim (the human
consented to a roster change, not to becoming a different agent mid-task) and
is deliberately out of scope here.

**The notice.** Both directions call the existing `deliver_to_orchestrator`
path — the same one `set_max_agents`/`set_autonomous` already use, not a new
delivery mechanism — with a `[loomux] workflow mode changed: …` line naming
the new state (workflow name and the armed gate, or "built-in roster, no merge
gate") so the orchestrator can revise its spawn/review strategy mid-session
instead of discovering the change on a bounced merge.

> **TODO (aggregate assembly):** the exact notice string, the refusal-message
> wording below, and the satisfiability audit key are Slice A's to finalize
> (`src-tauri/src/orchestration/mod.rs`, `workflow.rs`). This note describes the
> behavior the plan commits to; reconcile the quoted strings against Slice A's
> landed implementation before this lands on `main`.

### Three secondary outcomes

Each chosen so the launcher never has to invent a failure the engine doesn't have:

- **On, but the repo declares nothing.** A no-op, not an error — it is how you
  launch before you have written the file.
- **On, and the file is broken.** Audited, skipped, and the group launches on the
  built-in roster (a repo file may never stop a group from starting). So the
  launcher shows every finding as a **warning**, and Create stays enabled. A
  submit-blocking red box here would be the UI lying about what the backend does.
- **Off, but the repo declares a workflow.** Audited (`workflow-ignored`). A file
  that silently did nothing is the single most confusing thing this feature could
  produce, and the launcher says it too.

### The preview is the engine, not a second opinion

`orch_workflow_preview(repo, agent_cli)` runs the same `load_workflow` +
`Guardrails::clamped` that `create_group` runs, on a throwaway `Guardrails`, and
returns the resolved rows. It is deliberately **not** a second implementation of
the schema: a preview that disagreed with the launch would make the consent it
collected worthless. `the_preview_reports_the_roster_the_launch_would_actually_run`
asserts the two agree block for block.

(The workflow *pane* does validate the file independently, in TypeScript. That is
not a contradiction: the pane is an editor giving live feedback on text as you
type it, which cannot be a round trip to a backend that only reads files from
disk. The launcher is asking a different question — "what would you run?" — and
only the engine can answer it.)

The pure `src/roster.ts` holds what is left: the canonical role table (the union
and the badge text stay in `orchbadge.ts`; `launcher.ts` and `groupview.ts` had
each grown their own copy, and `groupview`'s had gone stale — it never gained
`planner`, so every planner showed a generic `AGENT` chip), and the resolution of
`(toggle, preview, per-role picks) → the roster that will run`. DOM-free, so the
four outcomes above are unit-tested rather than clicked through.

## Workflow-aware templates

The pipeline is prose (`templates/orchestrator.md`), not code — that was finding
#1 of the investigation. So "run **all** the declared reviewers on each PR" has to
be said in the prose, and it may only be said to a group that has them.

`render_template` is a dumb `{{KEY}}` replace with no conditionals, and it stays
that way. The conditional lives in Rust — `workflow::roster_is_custom(&blocks)`,
one predicate, used by everything — and the prose lives in markdown, where the
rest of the prose lives:

| Placeholder | In | Fragment | Empty when |
|---|---|---|---|
| `{{WORKFLOW}}` | `orchestrator.md` | `templates/workflow.md` | the roster is the built-in four |
| `{{BLOCK_NOTE}}` | `worker.md`, `reviewer.md`, `planner.md` | `templates/block.md` | *this block* is a built-in with no persona (and no reviewer siblings) |

Both placeholders sit **line-final**, at the end of an existing sentence, never on
a line of their own — a placeholder on its own line would leave a stray blank line
behind when it resolved to `""`, and "byte-for-byte unchanged" would be false by
one newline.

### Pinning that, for real

The first version of this pin was self-referential and rev-11 caught it (F1). It
built the expected value by taking the **live** template and replacing the
placeholders with `""` — which is exactly what production does when the toggle is
off, so both sides moved together. Unconditional prose added to a template passed.
A placeholder moved onto its own line passed. It was a test that the *gating* works,
wearing the name of a test that the *text* is unchanged.

What replaced it:

- **Golden fixtures.** `tests/fixtures/pre222/{orchestrator,worker,reviewer,planner}.md`
  are byte copies of the four templates from the commit before the toggle. The pin
  renders **those** with the six pre-#222 variables and diffs the result against what
  a toggle-off group is actually written. Any edit to a role template that changes
  what a default group reads now fails until a human re-blesses the fixture — and
  the diff on that directory becomes the review surface for "what did we just tell
  every worker to do differently?".
- **Placement asserted on the template source.** `{{WORKFLOW}}` / `{{BLOCK_NOTE}}`
  must each appear exactly once, be preceded by a non-newline character, and be
  followed immediately by a newline. That is the invariant the empty case rests on,
  and it is a one-keystroke mistake to break (wrapping a long line).

`a_workflow_placeholder_must_sit_at_the_end_of_a_line_it_shares` also asserts that
the live template differs from its golden by *nothing but* the placeholder, which
keeps "the fixture is stale" and "someone edited a template" distinguishable.

Two smaller decisions worth recording:

- **The one repo-authored string that reaches a template is defended twice.** A
  block's `name` is substituted **last** in `block_note`'s var list (and
  `{{BLOCK_NOTE}}` itself last in the caller's), because `render_template` walks its
  list in order and a value that goes in last has no pass left to rescan it. That
  ordering was originally claimed for the outer render only, and rev-11 found the
  gap: inside `block_note` the name went in *third*, so a block called
  `{{LANE_NOTE}}` was substituted in and then expanded — splicing loomux's own lane
  note into the middle of a sentence in a file the agent is told to read (bounded —
  only loomux's fragments were reachable, never attacker text — but prose corruption
  from a repo string, and a lie in this document). Now the name goes last **and**
  `sanitize_display` strips `{` and `}` outright. The order protects this template;
  the sanitizer protects the next one somebody writes.
- **The block note is per-block, not per-group.** A plain built-in `worker`
  sitting in a roster whose *reviewers* are custom has had nothing about its own
  identity changed, and telling it otherwise is noise in a file the agent is
  expected to actually read. The exception is a reviewer with siblings: being one
  of several focused reviewers *is* a change to how it should review, so it gets
  the lane note ("review **only your lane**; `rev-tests` is covering the rest")
  even with no persona of its own. That note is the difference between three
  focused reviews and three copies of the same generic one.

What the orchestrator's section says, and deliberately does not say: spawn by
**block id** (`spawn_agent(block: "<id>")`, not by kind — the file decides the
CLI, model and persona); run **every** reviewer block on each PR; treat a declared
gate as a **hard precondition** on merging, enforced by loomux rather than by good
intentions. And then the asymmetry the whole design turns on — **edges are
advisory**. Every scheduling call stays the orchestrator's. The file declares the
roster and the gates; the orchestrator routes.

The gate wording is kept generic on purpose: gate *enforcement* is sub-PR 3's, and
the template must depend on the fact that gates are enforced, never on how.

## The merge gate: verdicts as state (#222, closing the loomux half of #197)

An edge is advisory. **A gate is enforced** — and this is the part of the feature
nobody else in the survey ships. LangGraph, CrewAI, AutoGen and every node-canvas
tool leave "did the reviewer approve?" as a critic agent plus a magic termination
string; claude-flow ships consensus *agent prompts* (byzantine, raft, gossip) with
no enforcing runtime at all. loomux already owns the machinery that makes a gate
more than prose: the `gh`/`git` PATH shim, which refuses the merge *mechanically*
rather than asking an agent nicely.

"Mechanically" is not "unconditionally", and the difference matters — see **The
bypass surface, honestly** below. The gate constrains an agent that plays by the
PATH and by loomux's trust model, which is every agent loomux actually runs. It is
not a sandbox.

### Why `report()` could never be the gate

`report("done", "approved — looks good")` is a **notification**: untyped text typed
into the orchestrator's pane. That is exactly how PR #151 merged on the first
"approve" that arrived while a second, dedicated review was still running — and it
was the second review that found a real release-gate bypass (#196). The review
discipline worked; the merge jumped the gate before it finished.

A gate cannot key off a notification. It needs **state**: durable, attributed to
the reviewer that recorded it, and readable by something that can refuse a merge.
That is one new MCP tool and one new file tree.

### The verdict

    review_verdict(pr, verdict, summary)      # reviewer-kind blocks only

**A verdict is not a boolean.** Dify's Human Input node and Windmill's
`resume[...]` both give each decision its own outgoing edge and keep the approver's
typed input readable downstream; ours does the same:

| verdict | means | effect on the gate |
|---|---|---|
| `pass` | reviewed, nothing blocking | the only verdict that satisfies a gate |
| `fail` | blocking findings | refuses the merge |
| `escalate` | *not deciding* — ambiguous requirement, out of its depth, a risk it won't sign off on | refuses the merge |

`escalate` is the one that earns the model. Forced into a pass/fail bit, "a human
should look at this" becomes either a false approval or a false defect report.
Here it is a first-class outcome, and the summary that comes with it is what a
human actually reads.

Three rules, all from #197:

- **Blockers beat approvals.** One `fail`/`escalate` refuses the merge whatever the
  others recorded and whatever the threshold says — checked *before* any counting.
  First-to-report must never win.
- **The named reviewer's verdict is the gate**, not the first approval that turns
  up. A verdict from a reviewer the gate doesn't name satisfies nothing.
- **A verdict binds to a revision, not to a PR number** — next section.

Re-recording replaces that reviewer's own verdict — the `fail` → worker fixes it →
`pass` loop — and every write is audited, so the history is in the trail even
though only the latest verdict gates.

### A pass does not survive a re-push

Each verdict stores the PR's **head commit at record time** (`headRefOid`, captured
by the tool), and the gate compares it against the PR's current head. A `pass`
recorded against an earlier commit is **stale**: it counts as outstanding, not as a
pass, and the refusal names both the reviewer and the revision they must look at.

Without that binding the gate has a hole big enough to drive #197 through:

1. `rev-security` and `rev-tests` both pass PR #7 → gate satisfied.
2. The worker pushes two more commits ("fixed lint", "one more edge case").
3. `gh pr merge 7` → still satisfied. Those commits merge with **no reviewer having
   seen them**, through a gate reporting green.

Every requirement #197 states is met to the letter there ("every required verdict is
recorded PASS") and its actual point — don't merge code nobody reviewed — is
violated. It is the same failure GitHub's own review model closes by dismissing
stale approvals on new commits. Found in review of the first draft of this PR, which
keyed verdicts to the PR number alone.

Consequences worth knowing:

- A **blocking** verdict is *revision-independent*: a `fail` recorded against an
  older commit still refuses the merge. "This PR has a defect" doesn't stop being
  true because the author pushed more code; the reviewer clears it by re-reviewing.
- A verdict loomux could **not** bind to a commit (gh unavailable at record time)
  stores an empty head, which can never equal a real one — so it reads as stale
  rather than as "unbound, therefore fine".
- If the *current* head can't be resolved at merge time, the gate **refuses**: with
  no revision to compare against there is no way to know what any pass covers. Same
  fail-safe the human gate takes on an undeterminable base.
- Practically: don't send a worker back to "just tidy one thing" on an approved PR
  and expect it to merge. Send the reviewer back too. Both role templates say so.

### The gate

    gates:
      merge:
        require: all-pass        # or: threshold: 2
        reviewers: [rev-security, rev-tests]
        also: [ci-green]

`all-pass` (the default when `require:` is omitted) needs every named reviewer to
have recorded a `pass` — so **a reviewer that has recorded nothing keeps the gate
shut**, which is literally the #151 bug. `threshold: N` needs N passes and does
*not* wait for the reviewers it doesn't need: an author who writes `threshold: 2`
over three reviewers has said in the file that two are enough. They still cannot
merge over a `fail`.

`also:` names extra conditions. **`ci-green`** is checked in the shim with
`gh pr checks` (which exits non-zero when a check is failing, still running, or
absent). Anything this build does not know how to check **fails closed** — the
merge is refused, with the condition named, and audited. That asymmetry is
deliberate: a gate is a safety claim, and silently ignoring a clause of it would
turn a stricter-looking workflow file into a weaker one, which is the worst thing a
gate can do. (#197 Scope A's other condition, `no-live-agents-on-pr`, is therefore
*declarable but not yet enforceable* — it refuses every merge until a build knows
it. See **Still to come**.)

### How it composes with the human gate

The workflow gate is an **additional necessary condition**. It never opens a merge
by itself, and nothing opens *it* but the verdicts:

    gh pr merge
      │
      ├─ no LOOMUX_GROUP_DIR ── a merge loomux cannot gate ───────────── REFUSE
      │
      ├─ workflow merge gate  ── declared in .loomux/workflow.yml ────── REFUSE unless satisfied
      │                          (verdicts for the CURRENT head,
      │                           + also: conditions)
      │                          ↑ checked FIRST — no grant, no autonomous
      │                            marker, no dangerous mode can satisfy it
      └─ human merge gate     ── default branch only (#83) ───────────── REFUSE unless
                                 autonomous+auto_merge, dangerous mode, or a one-time grant

That order *is* #197 Scope B — *"an auto-merge must be structurally impossible
until every required review verdict is recorded PASS"*. A gate a human grant could
override would not be that. Two consequences worth stating:

- The workflow gate applies to **every** merge of the PR, not only to the default
  branch. The reviewers reviewed *that PR*; where it lands doesn't change whether
  they finished. (The human gate stays default-branch-only, unchanged — an
  integration-branch merge is still ungated *by it*.)
- A refused merge does **not** consume the human's one-time grant: the workflow gate
  exits before the grant is read, so nobody has to re-approve a merge that never
  happened.

### Findings disposition: a `pass` is not a disposition (#222)

The gate answers *"did the reviewers finish?"*. It cannot answer *"was what they found
dealt with?"* — and the first live run of this feature found the gap between the two.

The shape of it, from the human's dogfood run: a worker shipped a `divide()` with a
zero-guard. Both reviewers recorded **`pass`** — and both, in the same breath, posted the
*same* non-blocking finding: `b === 0` is bypassable by coercion, so `divide(5, '0')`
still returns `Infinity`, which is precisely what the change's own rationale ("fail loud
instead of propagating `Infinity`") said it existed to prevent. The orchestrator relayed
the finding to the human as an open question and then, when the second `pass` landed,
merged it under supervised dangerous mode — before the answer came, with the finding
unaddressed. Every gate was green. The feature shipped weaker than the issue asked for,
and two reviews' worth of feedback went in the bin.

Nothing there was a bug in the gate. The gate did its job: it counts verdicts, and both
verdicts were `pass`. The failure was **policy** — so the fix is policy, in the templates
rather than in the shim (`templates/orchestrator.md`, `templates/reviewer.md`,
`templates/workflow.md`, and `mechanics_core(Reviewer)` for replace-mode personas). Four
rules, and they are what the golden fixtures were re-blessed for:

- **Pass-with-findings is not "done" — it opens a disposition step.** The default
  disposition of a non-blocking finding is *fix it in this PR*: route it back to the
  worker before the merge. These are usually minutes of work, and they are the signal
  that compounds. Deferring is the exception and it is never silent — it costs a reason
  that says why the fix does not belong in *this* PR (a category word like "scope" is not
  one), a follow-up issue, and a line to the human. Filing that issue **parks** the
  finding rather than discharging it: it lands in the same label funnel as everything else
  (`agent-ready` is the human's go button, and the orchestrator may not pull an unlabelled
  issue), which is exactly why the line to the human is part of the price. The loop is
  bounded like the CI gate — three rounds of findings on one PR and it settles rather than
  ping-ponging, because a review loop that never terminates never ships the fix either.
- **Severity is the reviewer's rating; the requirement is the orchestrator's.** A finding
  that contradicts the change's *own stated rationale* is blocking regardless of the label
  the reviewer put on it, because a change that doesn't do what it claims hasn't met the
  issue — and the orchestrator, not the reviewer, owns that call.
- **Label and verdict move together.** A blocking *finding* means a `fail`/`escalate`
  *verdict*, never a `pass` that mentions it. Without that rule the new vocabulary would
  reopen the very hole it was added to close: a reviewer could label a finding blocking,
  record `pass`, and the gate — which reads verdicts, not prose — would open on a change
  its own reviewer called wrong. An approval with findings open is only ever an approval
  with *non-blocking* findings open, and its summary has to say so (`"pass — 2
  non-blocking, disposition pending"`): the verdict is *state that something merges on*, so
  a summary that reads like a clean bill of health is how feedback dies at the gate.
- **Hold on an open question — and know what a question is.** If the orchestrator asked the
  human to *decide* something about a PR, the merge holds until they answer, explicitly
  including when auto-merge, a one-time grant, or supervised dangerous mode would otherwise
  authorize it: those authorize a merge you were *ready* to make; none of them is the
  answer. But **telling is not asking** — a deferral the orchestrator decided, a status
  line, an audit announcement hold nothing, or the policy would deadlock on its own
  required deferral notice (and agents phrase decisions as confirmations: "deferring the
  nit to #240 — sound OK?" must not be a merge hold). Answered means *decided*, including
  "your call", which decides it by handing it back. A question never answered simply leaves
  the PR open — a correct outcome, not a stall — held visibly on the board and re-raised on
  each open-PR sweep, so it can't rot into a PR nobody merges.

The through-line, and the standing posture the orchestrator template now states outright:
**the orchestrator is the codebase's advocate, and merge speed is never the tiebreaker
against maintainability.** Autonomy is making that call unprompted — not taking the
shortest path to green.

### Where the state lives

Both artifacts are small files in the group's state dir, because the enforcement
point is a POSIX shell script with no `jq` — and because the gate state the shim
already reads (`autonomous`, `auto_merge`, `merge_grants/pr-<N>`) is exactly this
shape:

    <group-dir>/merge_gate                    # the declared gate, `key value` lines
    <group-dir>/verdicts/pr-<N>/<block-id>    # line 1 = pass|fail|escalate
                                              # line 2 = the head commit it reviewed
                                              # then: ts, agent id, summary

The verdict word is line 1 and the reviewed head line 2 — that *is* the shim's read.
The durable record and the enforcement input are **one artifact**, so they cannot
drift. Every token in `merge_gate` is already shell-inert: block ids and conditions
are *rejected* — never rewritten — by the parser when they leave their alphabet
(`sanitize_id` / `sanitize_condition`), which is the contract the parse boundary
established for precisely this consumer.

Four fail-closed rules govern reading those files. Each exists because the
alternative silently *weakens* a gate — or, in the last case, silently enforces a
rule the file never stated:

- **One verdict-token definition.** `Verdict::parse` is lowercase-strict, because
  the shim's `case "$v" in pass)` is a shell `case` and cannot be anything else. If
  Rust lowercased, a hand-edited `PASS` would read as satisfied to the orchestrator
  while the shim refused the merge — the two halves of one gate disagreeing about
  what a verdict *is*. Both now fail closed on it.
- **A truncated gate file refuses.** POSIX `read` returns non-zero at
  EOF-without-newline, so a final line with no `\n` is dropped by the loop — and a
  dropped `reviewer`/`also` line makes the gate *laxer*. `|| [ -n "$g_k" ]` is what
  keeps that from happening; a line the shim cannot parse at all refuses outright.
- **An unrepresentable token poisons the file** rather than vanishing from it. If a
  block id ever failed its sanitizer, dropping it from `merge_gate` would emit a
  gate one requirement short of what the repo declared. It writes an
  `unrepresentable` line instead, which nothing parses and which therefore refuses.
- **An unrecognized `require:` refuses, rather than defaulting to `all-pass`.**
  `all-pass` is the *strict* rule, so the fallback looked safe — but it would mean
  the shim enforcing a rule the file does not state, and agreeing with the Rust half
  (which calls that file malformed) only by luck. Two halves of one gate have to
  agree about what it *says*, not merely land on the same answer.

The decision itself is pure and unit-tested (`workflow::evaluate_merge_gate`); the
shim mirrors it in shell, and harnesses execute the *real* script against a fake
`gh` for every claim made here — including that a merge is refused under
`autonomous + auto_merge` and under supervised dangerous mode. A source-order
assertion would not do: a substring search still passes if someone hoists a marker
check above the gate block. The behaviour is what's pinned.

### The bypass surface, honestly

`doc/design/orchestration.md` → *Honest bypass surface* says of the human merge gate
that the shim "constrains an agent that plays by the rules — it is not a sandbox",
and lists calling the real `gh` by absolute path, a shell alias, or **forging a
grant file** as the shapes it does not close. **Everything there applies to this
gate too**, and the verdict store adds its own shapes. An agent with a shell can:

- **Forge a verdict.** `printf 'pass\n<head-sha>\n' > $LOOMUX_GROUP_DIR/verdicts/pr-7/rev-tests`
  satisfies the gate. The verdict dir is on disk under the agent's own uid, exactly
  like `merge_grants/`. What loomux guarantees is that no *MCP surface* lets a
  non-reviewer record one (enforced twice — dispatch and registry), not that the
  filesystem forbids it.
- **Delete the gate.** `rm $LOOMUX_GROUP_DIR/merge_gate` removes it for that group
  until the next launch re-reads the workflow file.
- **Unset the group dir.** `env -u LOOMUX_GROUP_DIR gh pr merge 7` used to skip the
  workflow gate entirely — with nothing in the audit, since there is no audit log
  without a group dir. **That one is now closed**: the shim refuses *any* merge with
  no `LOOMUX_GROUP_DIR`, matching what the human gate already did on the default
  branch. Every agent pane gets the variable and the shimmed PATH together, and a
  human's own shell has neither, so an unset variable at the shim is evasion rather
  than a supported flow. The remaining shapes above are the same class as
  absolute-path `gh`: closing them needs sandboxing, which is out of scope.

And the mitigation that closes the *human* gate does **not** close this one. A
machine account with no merge permission on the default branch means no client-side
evasion matters — the server refuses. But a machine account **cannot tell a forged
verdict from a real one**: to GitHub, a merge by an agent whose reviewers all
"passed" looks exactly like a merge by an agent that fabricated the files. This gate
is a *process* guarantee about loomux's own state, not an authorization boundary at
the forge. Branch protection with required reviews from real GitHub accounts is the
authoritative version of this idea; the workflow gate is the local, always-on,
zero-setup layer that catches the failure that actually happened (#151 — a
cooperating orchestrator merging too early), and the two compose.

### A gate lives and dies with the toggle that authorized it

A gate is part of the workflow, so it exists exactly when the workflow does — which
means **only when the human turned the advanced orchestrator on for that launch**
(*The advanced-orchestrator toggle*, above). Toggle off: the file is never opened, so
there is no gate, and the merge path is byte-for-byte what it was before #222.
Toggle back off after a gated launch and the gate is **cleared** — it must not
outlive the consent that authorized it.

On a **resume**, the gate is pinned to the launch exactly as the roster is, and the
argument is sharper for it. The roster pin exists so a `git pull` cannot swap a
delegate's persona under a session the human already approved; re-reading the file on
resume could just as easily *loosen* a gate — drop a reviewer, delete the clause —
under a session already running with it. The gate written at launch stands; drift is
audited (`workflow-changed-since-launch`), not applied.

Within a toggled-on launch, `merge_gate` tracks the repo, with one deliberate
asymmetry. Delete `gates.merge` (or the whole workflow file) and the gate is
**cleared** — a group must not keep enforcing a rule its repo has walked back. But a
workflow file that **fails to parse** keeps the last known gate, loudly
(`merge-gate-retained` in the audit). #225's rule — *a broken file is audited and
skipped, never fatal* — is right for the roster, where falling back to the built-in
four blocks still lets every agent spawn. It is exactly wrong for a gate: dropping
one because the file that declares it stopped parsing would quietly *widen* what
the group's agents may do, and a syntax error is not consent to merge unreviewed
code.

### The gate is a property of the session, not the PR (#316)

The rule above ("a gate lives and dies with the toggle that authorized it") was
written against a launch-time-only toggle. #316 makes the toggle live, which
raises a question the launch-time version never had to answer: what governs a
PR whose gate was armed under one toggle state, if the toggle moves before that
PR merges?

**Position taken: the gate is a property of the CURRENT SESSION, not the PR's
provenance.** Toggle off mid-session ⇒ the gate is off for every merge that
session attempts from then on, including a PR opened earlier while the
workflow was active. Toggle on ⇒ the gate is on, for every PR, regardless of
which toggle state it was opened under. This is not a new rule invented for
#316 — it is the *same* rule this section already states ("a gate lives and
dies with the toggle"), just confirmed to hold when the toggle moves live
instead of only at launch/resume.

The alternative — a gate that travels with the PR's provenance, so a PR opened
under a gated workflow stays gated even after the human turns the workflow off
— is the one that produces the surprise this feature exists to remove: "I don't
want to be surprised when I go to merge an item created in a custom workflow
and get rejected when I'm in a normal workflow." A provenance-carried gate is
exactly that surprise. A session-scoped gate, paired with the roster/gate
chrome always visible in the lifecycle UI (see the README's *Workflow
visibility* section), means the human always knows which rule is live before
they click Approve — session-scoped is simpler *and* is the unsurprising
answer.

One thing this does **not** change: a human "Approve" grant still never opens
the workflow gate on its own (#197/#222 — a grant is the *human* merge gate,
not the reviewer-consensus gate). A grant plus the workflow toggled off is what
lets the merge through; a grant against a still-armed gate still refuses.

**The refusal names three exits.** When a workflow-gated merge is refused,
telling the human only "blocked" repeats tonight's failure — the refusal has to
say what to do next, everywhere it can appear (the shim's refusal message, the
task board's Approve control, the groupview workflow row):

1. run the named reviewer blocks so the missing verdicts exist;
2. toggle the workflow off (session-scoped, so it takes effect immediately);
3. merge through the GitHub UI directly — the shim only gates `gh`/local `git`
   push-to-merge paths, not GitHub's own merge button.

> **TODO (aggregate assembly):** the literal refusal text (shim message +
> board tooltip copy) is Slice A/C's to write. This section states the
> requirement — three exits, always named — for the doc; reconcile against the
> shipped strings before merging to `main`.

### loomux never silently arms a gate the roster can't satisfy (#316)

Tonight's incident (see the plan comment on #316) was not a gate bug — it was
a **roster** bug wearing a gate's clothes. A group relaunched with the toggle
on, but with a broken or absent `.loomux/workflow.yml`: the *retained-gate*
rule above (correctly) kept the last-known gate naming `rev-orch`/`rev-ui`/
`rev-tests`, but the roster fell back to the built-in four blocks
(orchestrator/worker/reviewer/planner) — none of which can satisfy
`spawn_agent(block: "rev-orch")`. The gate was armed for reviewers the running
session structurally cannot spawn: unsatisfiable by construction, not by bad
luck, and nothing said so until a merge bounced hours into the session.

**The fix is a pure satisfiability check at every point a gate is armed** —
toggle-on, launch, and resume alike — not only a load-time nicety: does every
`reviewers:` name in the gate resolve to a block in the *current* roster with
`kind: reviewer`? If not, loomux does **not** silently widen its own promise by
dropping the gate (that would repeat the exact fail-open the retained-gate rule
exists to prevent) — it arms the gate anyway, marks it `satisfiable: false`,
and surfaces the mismatch loudly: an audit line naming the missing blocks, and
a chip in the lifecycle UI a human sees *before* the first merge attempt,
not after. Silence is what turned tonight's bug into an hours-long
half-workflow state; a loud, wrong-looking gate is recoverable in one glance.

> **TODO (aggregate assembly):** the pure check's name (the plan sketches
> `workflow::gate_missing_blocks(gate, blocks) -> Vec<BlockId>`) and the exact
> audit key are Slice A's. Reconcile this section's wording against what
> actually lands before assembling the aggregate PR.

### Where the reviewer learns about it

Nowhere in the base templates — and that is deliberate. A group with no workflow has
no gate, so gate prose in `reviewer.md` would be instructions about a tool that gates
nothing, in a file agents are expected to actually read. It would also have smuggled a
workflow-only contract into the default experience, which is exactly what the golden
fixtures exist to catch: the pin holds every default-group instruction file against a
checked-in copy (seeded from the templates as they stood before #222), so any change to
what a default group reads fails the suite until a human re-blesses it. That is a
review surface, not a freeze — the findings-disposition policy above is a deliberate
change to the default templates and was re-blessed as one. Workflow-conditional prose
still has no business there.

So the verdict contract rides on the two surfaces that exist *because* a workflow
does:

- **The reviewer's block note**, and only for a reviewer the gate actually **names**.
  It tells that block what the gate requires, who else it is waiting on, that a
  blocking verdict beats any number of passes, and that its pass goes stale on a
  re-push. The "does the gate name me" test is part of deciding whether the block note
  is written at all — a gate can name a plain built-in `reviewer` block with no persona
  and no siblings, and that block would otherwise be the one agent in the group that
  never learns its verdict is what the merge is waiting on.
- **`mechanics_core(Reviewer)`** — the non-overridable contract injected into every
  reviewer block. A custom block with a `mode: replace` persona never sees the built-in
  reviewer template at all, so without this the very population a gate is most likely
  to name would be the population that never heard of the tool.

The orchestrator learns it from the workflow fragment (`templates/workflow.md`), which
is likewise only rendered for a group whose workflow is in play.

## loomux runs its own workflow (sub-PR 5)

The feature ships with the repo using it. `.loomux/workflow.yml` at the root declares
loomux's own roster, and `.github/agents/*.md` holds the five personas it points at:

| block | kind | model | what it is for |
|---|---|---|---|
| `worker-deep` | worker | opus | work with judgment in it: a design with more than one defensible shape, a security/compatibility argument that has to be *made*, an honestly incomplete brief |
| `worker-quick` | worker | haiku | work whose shape is already decided: a rename, a version bump, applying a review finding that names the file and the fix. **Escalates instead of improvising** |
| `rev-orch` | reviewer | opus | the Rust backend: gate/shim security, capability closure, the `group_id` webview-trust boundary, the no-getrandom rule, integration-test-only linking |
| `rev-ui` | reviewer | sonnet | the vanilla-TS frontend: no framework, panes/overlays, **never resize the PTY**, DOM-free pure-module tests, xterm quirks |
| `rev-tests` | reviewer | sonnet | the tests *as tests*: intent vs implementation echo, the pin that cannot fail, cross-platform CI, the release path — and no live agent CLIs, ever |

Three things about it are decisions rather than filler:

- **The personas are files, not inline `prompt:`s** — and files in `.github/agents/`,
  Copilot's own convention. That is the one shape that is native on *both* CLIs (the
  matrix in *Personas: compiled to native flags*): `cli: copilot` loads it with
  `--agent rev-ui`, `cli: claude` compiles the same bytes into `--agents`. An inline
  prompt would have been Claude-only-native and unreviewable in a diff.
- **The block descriptions are the routing surface.** The orchestrator template already
  says to route with judgment; what it routes *on* is what each block says it is for. So
  the deep/quick split is written as a *deployment heuristic* (ambiguity and design →
  deep; mechanical and clearly-directed → quick), and `worker-deep` is declared **first**,
  because the first block of a class is what a bare `spawn_agent(kind: "worker")` resolves
  to and the safe default for an unrouted task is the tier that can handle being wrong.
- **The gate is `all-pass` over the three reviewers, plus `ci-green`** — and the reason is
  worth stating, because the first draft of this file said `threshold: 2` and a review
  (rev-14 F1) showed why that is wrong *for a lane-scoped roster specifically*. **An
  abstention is a pass.** A reviewer whose lane a PR doesn't touch is told to record
  `pass` ("outside my lane") rather than to stay silent, and the gate counts passes, not
  lanes. So on a backend-only PR the two out-of-lane reviewers — which are always the
  *fast* ones, having nothing to reproduce — satisfy `threshold: 2` while `rev-orch`, the
  only reviewer whose lane it is and the slowest by construction, is still running. The
  gate opens on two agents that said they had not reviewed the change, which is precisely
  the #151 failure the gate exists to prevent, dressed up as a quorum.

  `all-pass` costs nothing to fix that: the orchestrator already runs **every** reviewer
  block on every PR, and the out-of-lane ones pass in one turn, so the same three verdicts
  get recorded either way — `all-pass` just requires that the in-lane one is among them.
  The general rule this produces: **`threshold: N` is for *interchangeable* reviewers**
  ("any 2 of these 5 senior people"), and a lane-scoped roster is the opposite of
  interchangeable. Its other use — tolerating a dead reviewer — is a job for the human,
  not for the gate.

### A nudge toward cross-model review (#267 stage 1)

All three of loomux's own reviewers run `cli: claude` today — the lane split buys
overlapping-vs-*unique* findings across security/frontend/test-quality concerns, but
every lane is still read by the same underlying model family. A cross-tool review of
prompt-layer orchestrators (gstack, see the README's *Why loomux over…* comparison)
makes the case that a **second model** catches a different class of defect than the
one that wrote the code, independent of which lane it's reviewing — the workflow
schema can already express "reviewer block on a different CLI/model than the worker"
(`cli:`/`model:` are per-block, not per-workflow), it's just that nothing recommended
doing so. `.loomux/workflow.yml` now carries a comment above its `blocks:` reviewers
suggesting exactly that.

Deliberately **not done here**: flipping one of loomux's own reviewers to
`cli: copilot` in this file. That would change what the human's own live dogfood
session actually runs — it needs Copilot installed, and it changes this repo's
gate behavior for everyone, not just the reader of a doc. That's a one-line human
call (edit one `cli:` field in `.loomux/workflow.yml`), not something a docs PR
should assume on their behalf. Widening `SUPPORTED_CLIS` beyond claude/copilot
(gemini/codex adapters, for genuine reviewer diversity beyond the two CLIs loomux
already drives) is tracked separately as #267 stage 2.

The persona files deliberately carry **no `model:`**. Copilot would read one (it is its
key), loomux would not (the block's `model:` is its single source of truth), and two
spellings of one pinned model is precisely the silent-divergence bug this issue exists to
remove.

### Tiered models: what a block's `model:` is actually worth

The point of two worker tiers is that `model: haiku` reaches the CLI as haiku. That was
**verified end to end rather than assumed**, because a clamp that flattened a block's model
back to the group's per-role pick would have made the whole roster decorative:

    workflow.yml  →  parse_workflow      model: haiku kept (sanitize_model_opt is a
                                          character filter, not an allowlist)
                  →  Guardrails::clamped  sanitize_model(b.model, default_model(cli, kind))
                                          — the class default is the FALLBACK for an empty
                                          model, never a ceiling on a declared one
                  →  spawn_agent_ex       workflow::model_of(&block, agent_cli)
                  →  build_agent_command  `--model haiku`  (both CLIs — the flag is spelled
                                          the same for claude and copilot)

So **a guardrail model is a launcher default, not a ceiling**: the launcher's per-role picks
synthesize the *built-in* roster (and still fully decide it — `the_builtin_roster_still_honors_the_launchers_per_role_models`),
and a workflow file replaces that roster wholesale. `the_repos_own_workflow_runs_its_worker_tiers_on_the_models_it_declares`
pins the emitted command line for *this repo's actual file*, through the real load + clamp,
against launcher picks that say something else.

One resolution rule is worth stating plainly because it is the one people assume the other
way round: **a declared block with no `model:` takes its class default for its own effective
CLI — not the launcher's per-role pick.** The file is the roster; an undeclared field
resolves from the block. That keeps `orch_workflow_preview` honest, which is the whole
consent story — the preview resolves from `(file, group CLI)` and nothing else, so it cannot
disagree with the launch, and it *shows the human the resolved model of every block* before
they hit Create. Nothing is silent; it is simply the file's job to say what it wants. (Pinned
by `a_declared_block_model_survives_both_clis_and_a_resume`.)

### Keeping the file honest, forever

The repo's own workflow is validated by **both** halves of the feature, in CI:

- `the_repos_own_workflow_file_parses_clean_against_the_real_parser` (Rust) loads the real
  file through `load_workflow`, loads every persona through `load_block_profile` (which is
  also the kind-compatibility check), asserts each handle resolves back to its own file under
  `handle_resolves_to` (so a `cli: copilot` flip stays native), and asserts every `also:`
  condition is one this build can actually check — an unknown one fails closed, so shipping
  it would mean loomux could never merge its own PRs.
- `test/workflowdogfood.test.ts` (TypeScript) opens the same file in the **pane's** reader
  and validator and asserts zero findings — errors *and* warnings, because a warning here
  means the graph loomux draws of its own workflow has a block nothing points at.

Two parsers, deliberately (the pane is an editor giving live feedback on text; the backend is
the engine). A file only one of them accepts is a file the human is being lied to about, and
these two tests are what stop that drifting apart.

"In CI" was not true when this section was first written, and the fix was to make it true
rather than to soften the sentence: `ci.yml` ran `npm run build` (a **typecheck**, not the
suite), `cargo check` and `cargo test`, and **no `npm test` at all** — so the entire frontend
suite, this pin included, gated nothing. A change to `src/workflowmodel.ts` that made loomux's
own workflow file raise a finding in the pane would have merged green (rev-14 F2). `ci.yml`
now runs `npm test` on all three platforms, which is also what makes every other pure-module
test in `test/` a gate rather than a convention.

## Still to come

- **`no-live-agents-on-pr`** (#197 Scope A.1) — "no agent tied to this PR is still
  running" is the other half of the completeness check, and the gate schema already
  carries it. It needs a PR→agent binding loomux doesn't have today (the task board's
  `pr` + `assignee` fields are the obvious candidate, but they are orchestrator-
  maintained, so they are evidence, not proof). Until a build can check it, declaring
  it refuses every merge — which is the correct failure direction, and says so.
- **Verdict visibility for the human.** Verdicts are agent-facing state today: the
  human sees them in the audit log and in the orchestrator's pane. A per-reviewer
  verdict column on the board task (#197 Scope C's panel) is the natural next step —
  including which verdicts have gone *stale*, which the orchestrator can already read
  from `list_verdicts` but the human cannot see at a glance.
- **The forge-side gate.** Branch protection with required reviews from real GitHub
  accounts is the authoritative version of this idea, and the only one a forged
  verdict file cannot touch (see *The bypass surface, honestly*). loomux could help
  set it up; it can never substitute for it.
