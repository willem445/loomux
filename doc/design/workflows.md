# User-defined agent workflows: the block model

Issue #222. This note covers **sub-PR 1** — the backend core: roles as data,
`<repo>/.loomux/workflow.yml`, and compiling a block's persona down to each
agent CLI's native custom-agent flag. The workflow pane (sub-PR 2), the
`review_verdict` tool and gate enforcement (sub-PR 3), and the launcher's
"advanced orchestrator" toggle (sub-PR 4) build on what is described here.

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

## Nothing changes when there's no workflow file

The compatibility guarantee, and the thing most of the test suite defends: a
repo with no `.loomux/workflow.yml` gets a synthesized roster of exactly today's
four blocks — ids `orchestrator` / `worker` / `reviewer` / `planner`, no
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

## Still to come

- **sub-PR 2** — the workflow pane: `ContentPaneKind = "workflow"`, a text
  editor over the YAML plus a read-only derived graph (the Kestra/GitLab shape —
  the file is the source of truth and a GUI cannot corrupt it).
- **sub-PR 3** — `review_verdict(pr, verdict, summary)` as recorded,
  reviewer-attributed state, and gate enforcement in the `gh` shim: `gh pr merge`
  refused until every reviewer a gate names has recorded PASS. That is what makes
  a declared gate more than prose, and it closes the loomux side of #197.
- **sub-PR 4** — the launcher's "advanced orchestrator" toggle. Until it lands, a
  workflow file takes effect whenever one exists; the toggle will gate the load.
