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
less than the phrase suggests. `Role::containment()` is the exact answer — a
three-rung ladder (`Containment::None` / `NoEdits` / `ReadOnly`) selected by
class, never by a repo file. A **planner** is structurally read-only: its
file-editing tools *and* `git commit`/`git push` are denied at the CLI level, so
`is_read_only()` is a mechanical guarantee. A **reviewer** (#462) has its
file-editing tools denied too — but it keeps the shell, because running the tests
is its job, so its "never pushes" (and "never writes a file *via a shell
command*") stays *instruction-backed*. What the closed enum
guarantees is that **a repo file cannot change which posture a block gets**; it
does not claim any posture is a sandbox. `doc/design/orchestration.md`
("Reviewer containment: what is structural and what is not") draws the same
line in full, and this feature inherits it rather than tightening it.

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
| `cli` | select `claude` \| `copilot` \| `gemini` | validated against `SUPPORTED_CLIS` at parse *and* at spawn — and, since #267, against `CLI_CAPS`: a CLI that cannot enforce the class's containment tier is refused at both ends too |
| `model` | name a model | `sanitize_model` — the pre-existing allowlist filter |
| `effort` | select a thinking level | closed enum (`low`/`medium`/`high`/`xhigh`/`max`); rejected outright if it isn't in the vocabulary, **and** if the block's `cli:` has no `effort_levels` in `CLI_CAPS` |
| `context` | select a context window | closed enum (`1m`); same two-stage check. Composed into the model alias (`sonnet[1m]`) at emit, never stored in `model:` — `sanitize_model` strips brackets, so a `sonnet[1m]` written as a model id would silently become `sonnet1m` |
| `prompt` | free text | inert; sanitized, then delivered as a persona **addendum**, never as a replacement for the loomux contract |
| `profile` | name a repo file | confined to the repo (no `..`, no absolute path, no drive prefix) |
| `allow` | add tool patterns | **banned outright on a read-only class** (see below); inert for the rest — deny beats allow on both CLIs, so it can never re-grant what a class's tier denies |
| `id` | name the block | reserved: the four class names may only be used by their own class, so no block can hijack another's contract file |
| *(on a `kind: orchestrator` block)* | pin `cli` / `model` / `effort` / `context` only | `prompt:`/`profile:`/`allow:` are a **parse error** — the trust root is not a repo-writable surface (see below) |
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

A repo may pin the orchestrator's `cli`, `model`, `effort` and `context`. It may
**not** author its `prompt:`/`profile:`, and may not give it `allow:`. Both are
parse errors, and both are dropped-and-audited if they arrive from a hand-edited
`group.json` that never met the parser.

The pin list is not a list of "harmless-looking keys"; it is exactly **the picks
from a value set loomux already ships**, which is why #687's two knobs joined it
and why nothing else can. A closed-enum thinking level authors no text and
pre-approves no tool, so it opens no seam of the kind the paragraph below is
about: the worst a hostile repo buys is an orchestrator that thinks harder or
holds more context — and the human is shown the resolved value for every block
in the launcher's roster preview, before the toggle that reads the file at all.
The test of a candidate key is therefore not "is it small?" but "can its value
carry text the trust root will act on?" — `prompt:` can, `effort: xhigh` cannot.

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
list** — Edit, Write, NotebookEdit, `git commit`, `git push`
(`CLAUDE_EDIT_DENY_TOOLS`/`CLAUDE_READONLY_DENY_GIT`; see #448 — `MultiEdit`
was dropped from that list because it matches no real Claude Code tool, and the
list is now pinned in CI against the CLI's own documented tool set so a typo or
a stale name breaks the build instead of silently widening what a planner may
do). Deny beats allow on both CLIs, so an allow pattern cannot re-grant anything
*on that list*. But it doesn't have to: `allow: Bash(python *)` is named nowhere in the
deny list, and under `auto_ops` nobody approves the call — so the planner gets a
pre-approved shell that writes files, and the closure claim above becomes false.

Nobody can enumerate every write-capable program, so the rule runs the other way
round: **a read-only block gets no allow patterns, from any source.** The parser
rejects `allow:` on a read-only block (and says why); `persona_inject` drops any
that arrive from a `.github/agents` persona's frontmatter or a hand-edited
`group.json`, and audits the drop. For worker and reviewer `allow:` widens
nothing, and is just an approval prompt the author has chosen to skip: a worker
holds the whole surface outright, and a reviewer keeps its shell by design (so an
allow pattern names nothing it could not already run) while the editing tools
#462 denies it cannot be re-granted anyway — deny beats allow on both CLIs. The
ban stays keyed to the *fully* read-only class, where the argument above actually
bites.

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

### A Copilot persona's `tools:` is a filter, and it can strip loomux itself (#802)

The native column above has one trapdoor, and it is the whole of #802: a
`.github/agents/*.md` may carry a `tools:` frontmatter list, and Copilot's
`tools:` **filters** rather than adds. Per the [custom-agents configuration
reference](https://docs.github.com/en/copilot/reference/custom-agents-configuration),
*Tools processing*: *"The `tools` list filters the set of tools that are made
available to the agent - whether built-in or sourced from MCP servers"*, with
*"If no tools are specified, all available tools are enabled"* and *"An empty
tools list (`tools: []`) disables all tools"*.

So `--agent x` on a persona whose list omits loomux launches a delegate with
loomux's MCP **server loaded and every one of its tools filtered out** — an agent
that cannot `report`, cannot read the board, and cannot be steered. It presents,
from inside its own pane, as loomux being broken.

Three properties of that failure are worth keeping written down, because each one
sent a round of investigation the wrong way:

- **Only `profile:` blocks were affected.** loomux's own generated agent files
  have never carried a `tools:` key, so they inherit the documented all-tools
  default. The built-in roster on the same machine was fine, which read as "the
  workflow spawn path is broken" when the truth was "the file it points at is
  narrower".
- **No permission grant can undo it.** `--allow-tool loomux` and
  `permissions-config.json`'s `{"kind":"mcp","serverName":"loomux","toolName":
  null}` (see the orchestration note's *two permission surfaces*) both grant
  permission over what is *available*; `tools:` decides what is available at all.
  Filter first, approve second.
- **It is invisible from argv.** The command line is identical either way; the
  difference is inside a file in the user's repo.

The repair keeps #222's rule intact — loomux still never writes into
`.github/agents/`. Instead a persona whose list drops loomux is launched from a
**loomux-owned stand-in** in `~/.copilot/agents/` (a documented user-level agent
location, and `--agent` takes a name, which is what the stand-in supplies).

**A stand-in must not be a different persona.** It reproduces the user's
frontmatter *verbatim* and re-authors exactly three keys: `name` (it must be the
handle Copilot resolves the file by), `description` (Required, and loomux's own
bookkeeping label), and `tools` (the list, plus the grant). Everything else —
`model:` above all, but equally `infer:`, `target:`, and whatever Copilot
documents next — is carried as written. Dropping a key loomux has no opinion
about would change the persona's behavior as a side effect of a permissions fix,
which is the same class of silent substitution this whole area exists to prevent.

The grant uses the documented spelling (*"You can also explicitly enable all
tools from a specific MCP server using `some-mcp-server/*`"*), with a bare
`loomux` alongside as a hedge that costs nothing (*"All unrecognized tool names
are ignored"*) until a live run settles which the CLI matches on.

### loomux repairs an omission, never a decision

Three lists are reported and **left exactly as written**, because each states a
choice rather than an oversight:

| the list says | why loomux does not touch it |
|---|---|
| its own `mcp-servers:` | the stand-in models loomux's server and nothing else, so substituting would silently delete servers the user declared |
| `tools: []` | documented as *"disables all tools"* — a deliberate "nothing", not to be overruled into "nothing except loomux" |
| `tools: ["loomux/report"]` | the server is scoped per-tool on purpose; widening it to `loomux/*` would be loomux granting itself more than it was given |

A list that simply never mentions loomux is the omission — nobody writes
`tools: [read, edit]` *meaning* "and loomux must not work" — and that is the only
case rewritten. The distinction matters beyond tidiness: a tool that widens its
own capability whenever it finds itself under-privileged is exactly what #222's
capability closure forbids, and "it was for the user's own good" is the argument
that rule exists to refuse.

**Scoped to native personas only.** The filter can only bite where Copilot loads
the *user's* file, i.e. the native path. A `profile:` outside `.github/agents/`,
or one whose handle doesn't resolve back to its own file, is delivered by a
generated file that never carried a `tools:` key — its list was never in force,
so loomux neither warns about it nor starts reproducing it. Those blocks are
byte-identical to before.

And either way it is **loud**: an audit line (`copilot-persona-tools-gap`) and a
`NOTE:` on the `spawn_agent` reply naming the persona and the exact line to add.
The detection is not a nicety attached to the fix — the reason #802 cost three
rounds is that nothing failed audibly, so the same class must never be silent
again even where loomux repairs it.

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
  place that would have been a silent failure is a launch's *starter workers*
  count: a review-only workflow has no worker block, so those spawns would each
  have failed and the human would have gotten zero panes with only an audit line
  to explain it. The orchestrator is now told, in its pane. (Since #1020 the
  launcher asks for no starters at all, so the only caller that can still reach
  this is the promote modal — the notice is unchanged and still earns its place,
  because the count that reaches it is a human's either way.)
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
groupview chrome the toggle now shows — see `docs/orchestration.md`'s
*Custom agent workflows* section) can consent to a change the same way the
launcher preview does, without needing to tear the group down first.

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

The notice is `workflow_mode_notice`'s literal text
(`src-tauri/src/orchestration/mod.rs`): off reads `"[loomux] workflow mode
changed: built-in roster, no merge gate — re-plan your spawn/review
strategy."`; on reads `"[loomux] workflow mode changed: '<name>' active,
<gate clause> — re-plan your spawn/review strategy."`, where `<gate clause>`
is `merge gate requires all of|N of [<reviewers>]` plus a ` · `-joined
`also:` list when the gate declares one, or `no merge gate declared` when it
doesn't.

### Hot-reloading a manual file edit (#385)

The live toggle above only re-syncs the gate on a discrete human action
(flip it off, flip it on). Between #316 and #385, a **third** way the file
could get out of sync with the armed gate went unaddressed: a human hand-edits
`.loomux/workflow.yml` while `advanced_orchestrator` is already `true` and
never touches the toggle. Before #385, nothing ever re-read the file after
launch/resume in that case — the `merge_gate` spec the shim enforces stayed
whatever it was at the last launch or toggle flip, so an edit meant to loosen
an unsatisfiable condition (the incident that opened #385: a `ci-green` clause
in a repo with no CI to satisfy it) silently had no effect, and the human had
to merge manually.

The fix is a periodic background pass — `run_workflow_gate_reload`, on the
same `start_X`/timer shape as the idle reaper, the watchdog, the disk monitor,
etc. (`src-tauri/src/orchestration/mod.rs`'s "background loop" section) —
that, for every non-paused `advanced_orchestrator` group, re-derives the gate
from the CURRENT file and re-arms it through the *exact same* `sync_merge_gate`
call the fresh-launch and live-toggle paths already use. Not a second
gate-writing mechanism, just a third trigger for the one that exists: compare
the freshly-loaded gate against the one already armed (`self.merge_gate`,
which reads the spec file back), and only write + audit when they differ — an
unedited file costs one small read-and-parse and nothing else, no invalidation
state to track, and no audit-log spam every poll.

**Deliberately asymmetric with launch/toggle-ON, and the asymmetry is the
point.** A file that parses AND still names a `gates.merge` syncs exactly what
it declares, even if that *widens* what the gate accepts (drops a reviewer,
drops an `also:` clause) — a human's own edit is real consent for that gate's
shape. A file that stops parsing, or vanishes entirely, is treated differently
here than at launch: at launch "no file" is a legitimate zero-config choice
(there was never a gate to lose), while an *already-armed* gate whose file
goes missing or unreadable mid-session is indistinguishable from an editor's
ordinary unlink-then-recreate save — so the reload retains the last-known gate
rather than reading that blip as consent to open it. This is the "file deleted
mid-session" fail-closed case #385 called for explicitly; it self-heals the
moment the file is next readable and the reload sees it differs from what's
armed.

**Whether an agent editing `gates.merge` live should be trusted at all is a
separate, human-decided question — #459.** The human's call: an agent
updating the workflow is often legitimate and wanted, and the odds of an
agent adversarially weakening its own gate are low enough not to gate this
feature on it. So a valid edit to an *existing* `gates.merge` — including one
that loosens it — takes effect with no further human gesture required, and
#459 is the durable record of that accepted risk (so it stays revisitable,
not forgotten), not a design constraint this feature had to satisfy.

**But *removing* `gates.merge` entirely is not that same case, and #385/B1
found why the hard way.** `gates` — and every field inside a `Gate` — is
`#[serde(default)]`, the right behavior for a fresh launch (a zero-config
group is legitimate there) and the wrong one for a reload: a mid-flush
truncation that happens to land between two top-level keys (a
truncate-then-write save, a `git checkout` landing mid-flush) parses as a
perfectly valid document that simply never reaches `gates:` — byte-for-byte
indistinguishable, by type alone, from a complete file that genuinely
declares none. No amount of looking harder at that one document can tell
the two apart, and no amount of waiting can either: a truncation that
outlasts one poll tick outlasts two just as easily and just as silently, so a
multi-tick debounce is a mitigation, not a fix. The actual fix is a policy,
not an inference: **on the reload path, `gates.merge` being absent while a
gate is currently armed is never treated as removal.** It carries no
information either way, so it changes nothing. The one path that CAN take an
armed gate down to none is the explicit toggle-off — a single, discrete,
attributable action, never something a recurring background read infers from
whatever shape a file happens to be caught in. Belt-and-braces alongside that
policy, `reload_merge_gate_if_changed` also stability-checks every read (a
stat immediately before and after, both agreeing with what was actually
read) so a write actively in flight can't be misread as *any* kind of
change — narrower protection than the policy above (it only catches a write
caught in the act, not one that's paused or crashed mid-truncation), but free
and unconditional.

**Silence about the ignored removal is its own failure mode, and rev-33's
review named it.** A human who deletes `gates.merge`, saves, and sees nothing
happen has no way to tell "the reload is broken" from "the reload correctly
declined" — the same shape as the intake-gate incident where a suppressed
wake read identically to a lost one. So `reload_merge_gate_if_changed` audits
a `merge-gate-removal-ignored` line the moment it enters that state — but
only ONCE per transition (`merge_gate_removal_warned` is the latch, cleared
the moment the group leaves the state: the file regains `gates.merge`, or
the gate is actually cleared via the toggle), not on every tick the file
happens to still be gateless. A repeat audit every `WORKFLOW_GATE_POLL_
INTERVAL` would be exactly the log-spam this feature otherwise avoids for an
unchanged file; a LATER, genuinely new removal still audits fresh once the
latch has cleared.

**The toggle-off race.** `run_workflow_gate_reload` snapshots which groups
have `advanced_orchestrator` on before iterating them; `set_advanced_
orchestrator` runs independently, on whatever thread handles the Tauri
command a groupview click dispatches to. If a toggle-off completes on that
thread between the snapshot and this group's own turn in the loop, the
read + parse `reload_merge_gate_if_changed` does could still finish AFTER
the toggle-off — and, seeing the file still declare a gate the just-cleared
`merge_gate` file no longer does, try to re-arm it. That would wedge a gate
back onto a group that just explicitly turned workflow mode off. The safe
direction (more enforcement, not less) — but a real bug regardless, and
"it fails safe" is not a reason to ship a known race in security machinery.
`reload_merge_gate_if_changed` re-reads the group's guardrails immediately
before writing, and refuses if the toggle is no longer on — shrinking the
window down to the gap between that recheck and the write itself, which no
lock-free background pass can close entirely without a cross-thread lock
this codebase doesn't otherwise need.

Only the gate reloads — the roster (`guardrails.blocks`) stays
launch/toggle-pinned, for the same reason a live delegate's persona doesn't
change underneath it (above): reloading the roster live has a much larger
blast radius (already-spawned agents, in-flight worktrees) that #385 is not
scoped to. A gate whose reviewers the pinned roster can't spawn is audited
(`merge-gate-unsatisfiable`), never silently armed anyway — the same check
the live toggle already runs.

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

### …and the head SHA does not pin the PR body (#565)

The head oid pins the **code**. The **PR body is not part of it**, and on a repo
that squash-merges the body *becomes the permanent commit message*: reviewed
content with the weight of a diff and none of a diff's version pinning. It drifts
in both directions, and both were live in one batch:

- a reviewer passes a body, the author edits it, and the merge carries text nobody
  reviewed;
- a reviewer *fails* a body that has already been fixed, and the PR is blocked on a
  defect that no longer exists. That is #525, to the second: review comment posted
  at 14:44:23Z, body last edited at 14:47:49Z. The reviewer was accurate about what
  it saw and stale about what exists, and **no mechanism could have told either
  agent** — it cost a full round-trip on a PR that was otherwise ready.

So `review_verdict` records a **sha256 of the body** alongside the head, and four
decisions make it work:

**1. A digest, not the text.** A body here runs ~250 lines; storing a copy per
verdict is heavy and produces an artifact that still has to be diffed by eye —
which is the manual step that cost the round. A digest is fixed-size and its
mismatch is *exact*: same digest, provably the same commit message that was
approved. (The other cheap option, an `updatedAt` timestamp, is worse than
nothing: it moves for labels and assignees, so it cries wolf, and when it does
fire it says *that* something changed, never *what*.)

**2. The tool computes it; the reviewer never passes it.** This is #525's own
lesson turned on our tooling — *a property that depends on someone remembering is
an intention, not a mechanism*. The tool already has the PR number, so it fetches
the body itself. A reviewer cannot forget it, and cannot record the digest of a
body it did not read.

**3. Reported on both classes, enforced only on passes.** Symmetric invalidation
would ping-pong forever: fail on the body → worker fixes the body → verdict
auto-stales → re-review → repeat. So:

| verdict | body moved since | what happens |
|---|---|---|
| `pass` | the hazard: what would be committed is not what was approved | reported, and **refused** at merge time where the repo opted in |
| `fail` / `escalate` | expected — that *is* the fix loop | reported, and nothing else. The reviewer clears it by re-recording |

The reporting half is unconditional (`list_verdicts` carries `body_changed` per
verdict; the gate status line the orchestrator is handed says which class it is),
because it is the half that would have resolved #525 with no re-review at all:
*"this FAIL was recorded against body-digest X, current is Y"* is a race the
orchestrator can settle by reading the current body.

**4. Enforcement is opt-in — `also: [body-unchanged]`.** The check only matters
where the body *becomes* the record. On a repo that merge-commits, the PR body is
discussion, and this would be noise. "Squash makes the body permanent" is a fact
about *our* workflow, not about git, so it is a clause a repo writes down rather
than an assumption in the product (CLAUDE.md constraint 8). It rides the existing
`also:` mechanism, so a build that does not know the condition refuses every merge
declaring it — the same fail-closed rule every other condition gets.

What is digested is deliberately the *smallest* normalization POSIX shell can
reproduce, because both halves have to agree on it forever: **`\r` removed**
(whether `gh` hands back CRLF or LF is a platform fact, not content) and
**trailing newlines collapsed to one**. Nothing else — a re-wrapped paragraph *is*
a change, since the claim being made is "the bytes that will be committed are the
bytes that were reviewed", which is checkable, and not "the meaning is close
enough", which is not. Rust does it in `workflow::canonical_body`; the shim does it
with `tr -d '\r'`, `$(…)` and `printf '%s\n'`. The agreement is not asserted, it is
*executed*: `the_shim_refuses_a_merge_whose_body_moved_after_the_pass_when_the_repo_opts_in`
records a verdict through the real MCP tool and then merges through the real shim
over a body carrying CRLF, trailing blank lines, trailing spaces, non-ASCII and
`$`-bearing text — it can only pass if two independent implementations produced the
same 64 characters.

**What this deliberately does NOT cover: the PR title.** GitHub takes a squash
commit's *subject* from the PR title, which is as editable as the body — so the
squash record is pinned here only from the second line down. Covering it means the
shim joining two `--json` fields into one canonical string, which is a **second
canonical-form contract that the shell and Rust halves would have to keep agreeing
about forever** — the same class of coupling this section spent its whole design
budget keeping to two rules. That earns its own change with its own executed
cross-check, rather than riding in on this one. #565 stays open on exactly that
residual; it is the follow-up artifact, and nothing in the mechanism below has to
change to add it (one more field in `pr_body`'s query, one more line in
`canonical_body`, one more `--jq` expression in the shim).

The issue also names a planner's **issue body** as a candidate. That one is not a
smaller version of this: it is part of no commit, and no gate reads it, so it needs
its own argument for what a digest would be *for* before it gets one.

Fail-closed, like the head binding: a verdict with **no** digest (recorded by a
build older than this one, or with the body unreadable at record time) can never
show the body unchanged, so `body-unchanged` refuses on it. The hasher itself is
resolved at the point of use (`sha256sum`, else `shasum -a 256`, else
`openssl dgst -r`) rather than added to the shim's proven-dependency preamble —
that preamble is asserted before *every* gated command on every host and is shared
byte-for-byte with the git shim, so requiring a hasher there would refuse merges on
hosts that never declare this condition. No usable hasher, or output that isn't 64
hex characters, refuses *this condition* and says so.

### The gate

    gates:
      merge:
        require: all-pass        # or: threshold: 2
        reviewers: [rev-security, rev-tests]
        also: [ci-green, body-unchanged, base-green]
        max_diff_lines: 800      # optional; omit for no limit

`all-pass` (the default when `require:` is omitted) needs every named reviewer to
have recorded a `pass` — so **a reviewer that has recorded nothing keeps the gate
shut**, which is literally the #151 bug. `threshold: N` needs N passes and does
*not* wait for the reviewers it doesn't need: an author who writes `threshold: 2`
over three reviewers has said in the file that two are enough. They still cannot
merge over a `fail`.

`also:` names extra conditions. **`ci-green`** is checked in the shim with
`gh pr checks` (which exits non-zero when a check is failing, still running, or
absent). **`body-unchanged`** (#565, previous section) re-reads the PR body at
merge time and refuses if it is not the one the live passes were recorded
against — for repos that squash-merge, where the body is the commit message.
**`base-green`** (#1174, below) refuses while the branch the PR would land on
is itself broken. Anything this build does not know how to check **fails closed** — the
merge is refused, with the condition named, and audited. That asymmetry is
deliberate: a gate is a safety claim, and silently ignoring a clause of it would
turn a stricter-looking workflow file into a weaker one, which is the worst thing a
gate can do. (#197 Scope A's other condition, `no-live-agents-on-pr`, is therefore
*declarable but not yet enforceable* — it refuses every merge until a build knows
it. See **Still to come**.)

### The small-batch clause: `max_diff_lines` (#1174)

    gates:
      merge:
        reviewers: [rev-lead]
        max_diff_lines: 800

Refuses a merge whose PR changes more than `N` lines — additions plus
deletions, over the whole PR. The practice is small-batch delivery
(trunk-based development; Google's "small CLs"); the reason it earns a
mechanism *here* is that loomux's entire quality story is the review gate, and
an oversized PR is the canonical way an agent fleet defeats it — reviewers
rubber-stamp what they cannot hold in context. "Split it" was previously
reviewer prose. A declared threshold makes it mechanical.

**A structured key, not an `also:` token.** `also:` is a closed vocabulary of
*parameterless* conditions whose entire safety property is that every entry
either matches a known name or fails closed. A parameter cannot live in that
namespace: `max-diff-800` would be a token no build could recognise and every
build would therefore refuse. So the threshold is a key, and — like
`merge_queue:` before it — an older loomux fails the whole file's parse on the
unknown key rather than silently dropping the clause. That is the documented
consequence, not an oversight: a workflow file that says 800 must never load
as a workflow file that says nothing.

**Absent is off, and `0` is refused.** No key means the size is never asked
about at all, so every repo that has not adopted this is byte-for-byte on its
old path — including when the size would have been unreadable. `0` is a parse
error rather than a synonym for "unlimited": the way to mean no limit is to
omit the key, and a bound a repo wrote down must never be read as the absence
of one. A negative or fractional value never reaches that check; serde refuses
the whole file at `Option<u32>`, exactly as `threshold: -1` already does.

**Where the number comes from.** `gh pr view --json additions,deletions`, i.e.
gh's own JSON — deliberately *not* parsed out of `gh pr diff --stat`'s English
summary line. That line is prose ("1 file changed, 2 insertions(+)"), gh may
reword it, and it drops a clause entirely when a count is zero; a security shim
that has to word-split a sentence to decide a merge fails in a new way every
time that sentence changes. A size loomux cannot read **refuses** — an
unmeasurable PR is not a small one.

**One definition, three enforcement points.** `workflow::check_diff_size` is
the pure decision: the merge queue calls it, and the shim's shell mirrors it.
The queue matters here specifically because it never calls `gh pr merge` — it
fast-forwards a scratch ref — so a clause the shim alone enforced would leave
the queue as the way around the limit.

### Stop the line: `also: [base-green]` (#1174)

`ci-green` asks whether *this PR* is green. `base-green` asks whether the
branch it would land on is — refusing while the base ref's HEAD is red, still
running, or reports nothing at all. The practice is trunk-based "fix the build
first" (Toyota's Andon cord; it is also the premise behind GitHub's own merge
queue). Without it, nothing stops a fleet piling work onto a broken branch,
which compounds failures and hands the merge queue's bisect a culprit set it
cannot untangle. The base is resolved live at merge time, as the gate already
does for everything else.

**Two API endpoints, because one would be a lie on half of GitHub.** The
combined-status endpoint (`/commits/{ref}/status`) sees only the legacy Status
API; the check-runs endpoint (`/commits/{ref}/check-runs`) sees only check runs,
which is what GitHub Actions reports. A repo using either alone reports
*nothing* from the other, so "green" here means **neither surface said anything
bad, and at least one of them said something at all**. Green is an allow-list of
check-run conclusions (`success`, `neutral`, `skipped`), so a conclusion GitHub
adds tomorrow reads as red rather than as green.

**Unknown is never green, and the cost is stated.** A base HEAD with no checks
and no statuses refuses, matching what `ci-green` already does for a PR with no
checks reported and what the merge queue's `base-unverifiable` does for a base
it cannot resolve. The cost is real and belongs in the open: on a repo whose CI
can skip a commit (a `paths-ignore` filter, say), a base commit that ran nothing
refuses every merge onto it until something does run. That is why the clause is
opt-in — a repo that cannot promise its base always has checks should not
declare it.

**`gh api` has no `-R`.** Its `{owner}/{repo}` placeholders resolve from the
current directory's remote, which is the *wrong* repository whenever the merge
was invoked as `gh pr merge -R other/repo`. The shim therefore resolves
`nameWithOwner` explicitly (through `gh repo view`'s positional repo argument,
per #294) and refuses if it cannot. The merge queue keeps the placeholders: it
only ever operates on its own group's repo, and never has an `-R` to honour.

### The PR-open advisory, and which way each half fails (#1174)

The size clause has an advisory half as well as an enforced one, and they are
deliberately asymmetric:

| | when | channel | fails |
| --- | --- | --- | --- |
| advisory | `gh pr create` | the **author's** own pane (stderr) + an audit line | **open** |
| refusal | `gh pr merge` | the shim's refusal path | **closed** |

**Why the author's pane and not the orchestrator's.** The obvious reading of
"notify the orchestrator" has no honest implementation here. The shim is a
POSIX script in another process; the orchestrator's notice inbox
(`OrchNoticeInbox`) is in-memory in the Rust registry, and the only channel the
shim has into loomux at all is `audit.jsonl`, which nothing reads back. Building
one would mean a durable, agent-writable file whose contents are injected into
the orchestrator's MCP tool results — a prompt-injection channel straight into
the trust root, which is not a thing to add for a courtesy notice. The author's
pane is also the *right* target on the merits: the author is the actor who can
split the PR, and it learns at the moment the split is cheapest. The
orchestrator still sees it — the audit line, and the worker's own report.

**Why the advisory fails open.** It is a courtesy, not a gate. The extra
`gh pr view` failing, timing out, or a gate file that cannot be read must never
break or delay `gh pr create`, and the create always succeeds regardless of the
PR's size. Every enforcement decision in this document fails *closed*; this one
does not, because it decides nothing. The merge-time refusal is where the
"unknown is never safe" rule applies, and it applies there in full.

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
                                              # line 3 = ts     line 4 = agent id
                                              # line 5 = digest of the body it reviewed
                                              # then: summary, to EOF

The verdict word is line 1, the reviewed head line 2 and the reviewed body's digest
line 5 — `head -n1` / `head -n2 | tail -n1` / `head -n5 | tail -n1`, which *is* the
shim's read. Every fixed field sits above the summary because the summary is the one
field that may contain newlines. A file written before #565 has its summary where
line 5 now lives: `parse_verdict_file` hands a line 5 that is not a valid digest back
to the summary rather than swallowing durable prose, and reads the digest as
*unknown* — the shim, which takes line 5 as-is and accepts only 64 hex characters,
lands on the same gate decision (unknown → `body-unchanged` refuses) by a stricter
route.
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
- **Delete the gate.** `rm $LOOMUX_GROUP_DIR/merge_gate` removes it for that
  group — **but only for up to `WORKFLOW_GATE_POLL_INTERVAL` (30s), not until
  the next launch, as of #385.** If `.loomux/workflow.yml` still declares
  `gates.merge` (the common case — the file itself wasn't touched, just the
  generated spec), the next background reload sees the declared gate differ
  from the now-missing armed one and re-writes it, self-healing the deletion.
  Deleting the workflow FILE itself (not just the spec) doesn't reopen this
  bypass either — #385/B1 makes that retain the last-known gate rather than
  clear it. The only way this bullet's original "until relaunch" window
  still applies is turning workflow mode off first (which clears the gate
  deliberately) or disabling/killing the background reload thread, which is
  a much bigger compromise than deleting one file — **or pausing the group,
  the next bullet.**
- **Pause the group.** `touch $LOOMUX_GROUP_DIR/paused` has no LIVE effect —
  `run_workflow_gate_reload` (and every other background pass) checks the
  in-memory `paused` set, not this file, and `pause_group`/`resume_group`
  aren't reachable from any MCP surface an agent can call, only a human's
  own groupview click. But the marker IS re-seeded into that set the next
  time the group RESUMES (`create_group_ex`'s `if dir.join("paused").is_file()`
  re-hydration — "a pause is a durable human safety action" surviving a
  restart, the same contract every other guardrail marker gets). So combined
  with the previous bullet — delete `merge_gate`, then drop this marker too
  — an agent turns the ~30-second self-heal above into an outage that lasts
  until the group's next resume finds the marker gone (it won't, since the
  agent left it there) or the human notices the group reads paused and looks
  closer. Same class as the other two bullets: a filesystem write under the
  group dir an agent's own shell can already make, not a new API surface.
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

**On a resume, AT THE MOMENT OF RESUME, the gate is not re-derived** — same as the
roster, and for the same reason: `create_group`'s resume arm does not re-read
`.loomux/workflow.yml` to decide what gate this session runs under; it keeps
whatever `merge_gate` already has on disk. **This is no longer the whole story
once #385 shipped, and the difference matters.** Before #385, "not re-derived at
resume" meant frozen for the rest of the session too, because nothing else ever
re-read the file after launch either — a `git pull` between launch and resume
could never loosen the gate (drop a reviewer, delete the clause) without a
further explicit human action. That guarantee is gone: `run_workflow_gate_reload`
(*Hot-reloading a manual file edit*, below) treats a resumed group exactly like
any other live advanced-orchestrator group, re-deriving its gate from whatever is
CURRENTLY on disk within `WORKFLOW_GATE_POLL_INTERVAL` of the resume completing,
git pull or hand-edit, loosening or tightening, with no further consent moment.
That's a decided, accepted risk (#459) — see the section below for the argument
and for the one thing that's still absolute regardless: fail-closed. Drift
against the file that predates a resume is still audited
(`workflow-changed-since-launch`) independent of any of this.

Within a toggled-on launch (or a live toggle-ON), `merge_gate` tracks the repo AT
THAT MOMENT, with one deliberate asymmetry that still holds at launch/toggle
time: delete `gates.merge` (or the whole workflow file) and the gate is
**cleared** — a group must not keep enforcing a rule its repo has walked back,
and turning the toggle on IS a fresh consent moment. But a workflow file that
**fails to parse** keeps the last known gate, loudly (`merge-gate-retained` in
the audit). #225's rule — *a broken file is audited and skipped, never fatal* —
is right for the roster, where falling back to the built-in four blocks still
lets every agent spawn. It is exactly wrong for a gate: dropping one because the
file that declares it stopped parsing would quietly *widen* what the group's
agents may do, and a syntax error is not consent to merge unreviewed code.
**The background reload (below) does NOT share the "delete it and it clears"
half of this asymmetry** — that's #385/B1's own finding: on the reload path
absence is never removal, full stop, precisely because a reload can't tell a
deliberate deletion from a mid-write truncation the way a one-shot launch/toggle
read can afford to assume it can.

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
chrome always visible in the lifecycle UI (see `docs/orchestration.md`'s
*Custom agent workflows* section), means the human always knows which rule
is live before they click Approve — session-scoped is simpler *and* is the unsurprising
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

The shipped text: the shim and the Rust-side status line share
`GATE_REFUSAL_EXITS` (`src-tauri/src/orchestration/mod.rs`) verbatim —
`"Three ways forward: (1) get the named reviewer(s) to run and record a
verdict, (2) have the human turn workflow mode off for this session (clears
the gate), or (3) merge this PR from the GitHub UI, which is not gated."`
The board tooltip is its own wording of the same three exits —
`gateExitsMessage()` (`src/workflowstatus.ts`) — since a shell string and a
TypeScript string can't share one constant.

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

The pure check landed exactly as sketched —
`workflow::gate_missing_blocks(gate: &Gate, blocks: &[Block]) -> Vec<BlockId>`
— and the audit key it feeds is `merge-gate-unsatisfiable`.

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
| `rev-orch` | reviewer | opus | the Rust backend: gate/shim security, capability closure, the `group_id` path boundary (#904: one validated `GroupId`, one assembly point), the no-getrandom rule, integration-test-only linking |
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
was #267 stage 2, below — **gemini** is now spawnable, so the nudge above has a
genuinely different model family to point at.

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

### An undeliverable knob's refusal is an authoring rail (#782)

The `effort:`/`context:` caps check has two audiences and only one of them has a UI. A human
picking a knob in the launcher gets it greyed out with the vendor reason attached, from
`agent_cli_knobs` — same `CLI_CAPS` rows the parser consults, so the two cannot disagree. An
**agent** authoring the file has no such surface: the parse error is the entire rail, and a
verdict ("cli \"copilot\" cannot set effort") leaves it choosing between deleting the key and
rewriting the block's `cli:` by guesswork. It reported the resulting YAML errors as a loomux
bug, which is the right read — a refusal that doesn't say what to do instead is an
unfinished refusal.

So `validate_knob` names the block, the knob, the exact value, the vendor reason, and both
escapes. The escape list is *derived*: it asks every `CLI_CAPS` row loomux can spawn whether
it carries that value. No CLI is named in `workflow.rs`, wiring a knob on another CLI updates
the message with no edit, and — per CLAUDE.md constraint 8 — the product code stays free of
per-vendor special-casing. The skill (`.claude/skills/author-loomux-workflow`) carries the
matrix as a snapshot and says plainly that `CLI_CAPS` wins when the two disagree.

The loudness is only affordable because a refused file cannot wedge anything: `load_workflow`
returns the errors, the launcher preview reports `valid: false` with them attached, and a
launch falls back to the built-in roster. That property is now pinned by its own test rather
than left as a comment — if a broken file could block a spawn, this check would have to be
silent, which is the failure mode #687 existed to remove.

## Intake as data: the `intake:` block (#382 P1)

`.loomux/workflow.yml` gained a second top-level block, `intake:`, the missing
sibling of `gates:` — "where work comes from" beside "what gates it". This
section documents the slice as it landed: **schema, parsing, resolution and
persistence only.** Nothing yet renders it into a template, nothing yet reads
it in `gh.rs`'s label allow-list or `idle_tick_notice()`, and the
`board`/`none` sources have no runtime. Those are P2 (template
parameterization, serialized behind #329/#398's golden re-bless), P4 (consumer
rewiring) and a Phase B follow-on, each its own reviewable PR. What this slice
guarantees is that a resolved profile exists and is stable, so those later
slices — and #332's host-side intake poller, landing in parallel — have a
single, persisted contract to build against instead of re-deriving one.

```yaml
intake:
  source: github-labels        # github-labels (default) | board | none
  labels:
    ready:       agent-ready            # "build this"
    investigate: agent-investigation    # "look, don't build"
    owned:       agent-managed          # "mine" ownership marker
    prototype:   agent-prototype        # demo-gated; optional
    hold:        agent-hold             # human veto — never start this (#778)
```

Every field is optional and falls back to the built-in default independently —
a repo can override `labels.ready:` alone and inherit `investigate`/`owned`/
`prototype`/`hold` unchanged, and can omit `intake:` entirely, which resolves
to `workflow::builtin_intake_profile()` exactly.

`hold:` is the one label whose meaning is a *veto* rather than a selector: it
names the label the #332 host poller must treat as "the human said no" when a
group runs in full autonomy, where the start default inverts and every open
issue is otherwise eligible (#778). Its spelling is read from the resolved
profile at poll time, not from a const — a repo that renamed the label must
still have its vetoes honored.

### Why it is inert vocabulary, not a capability

Same argument as `blocks:` (*Capability closure*, above), applied to a smaller
surface: `source:` selects from a closed three-value enum (`kind_from_str`'s
"reject, never coerce" shape — `intake_source_from_str`), and every label is
sanitized through `sanitize_id`, the same conservative `[A-Za-z0-9_-]`
alphabet a block id gets, rejected rather than rewritten so an author's typo
surfaces as an error instead of a label their repo's real GitHub labels no
longer match.

**There is no spelling under `intake:` that can disable the human merge
gate.** That gate lives in the `gh` PATH shim, keyed to group markers
(`autonomous`, `merge_grants/pr-<N>`, …) — not to this file — and `intake:`'s
wire type (`RawIntake`, `RawIntakeLabels`) carries `deny_unknown_fields` with
no `human_gate:`-shaped field defined on it at any nesting level. A
`human_gate: false` (top-level, under `intake:`, or under `intake.labels:`) is
therefore a hard parse error by construction, not a line this schema has to
specifically recognize and refuse — the same guarantee `gates:` already gives
the merge gate it *does* declare (a `gates.merge` clause is data the shim
reads; nothing in `gates:` can waive the marker-keyed human gate either).
`intake_human_gate_spelling_is_a_deny_unknown_fields_error` and its siblings
in `tests/workflow.rs` pin this at every nesting level the schema offers.

### The golden self-reference trap, dodged early

`builtin_intake_profile()` is a checked-in function returning a fixed,
independent value — not derived from the parser, not derived from anything
`parse_workflow` does with an absent `intake:` key. This is deliberate, ahead
of P2's byte-golden fixture work: if the const *were* built by asking the
parser to resolve an empty file, a bug in "what absent resolves to" and a
matching bug in "what the const says" could move together and the tests would
still pass — the exact self-reference bug rev-11 F1 found in the pre-#222
golden pin. Keeping the const and the parse-time default derivation as two
independent expressions of "today's vocabulary"
(`builtin_intake_profile_matches_todays_github_label_vocabulary` pins the
const's bytes directly) means either one drifting from the other is a visible
test failure now, not a landmine for whoever writes the P2 byte-golden.

### Resolution and persistence — the same gate `blocks` rides

`Guardrails` gained `intake: workflow::IntakeProfile`, resolved and gated
**exactly like `blocks`**: a repo's declared `intake:` block takes effect only
on a fresh launch (`Launch::Fresh`) with `advanced_orchestrator` on — the
moment the human has been shown the launcher preview. A resume never re-reads
the file; the profile in `group.json` (a `"intake": { "source", "labels" }`
object beside `"blocks"`) is what runs, pinned for the same consent reason
*A resumed group runs the roster it was launched with* argues for the roster:
nobody is shown anything at resume time, so a `git pull` between launch and
resume must not be able to swap the intake vocabulary a session is already
running under.

One deliberate difference from `blocks`: the resolved profile is **available
regardless of the toggle** — `Guardrails::default().intake` is the built-in
profile, so a group running the built-in 4-block roster (advanced orchestrator
off, or on with no declared file) still has a well-defined intake profile for
#332's poller to read. Toggling advanced mode on decides whether a repo's
*override* takes effect, not whether a profile exists at all.

Absent `intake` in an old `group.json` (one written before this field
existed) resolves to the built-in default on read — the same migration
guarantee `#[serde(default)]` gives `blocks`.

The resume-drift audit (`audit_workflow_drift`) compares intake alongside the
roster, not just the roster: a repo can rename its label vocabulary without
touching a single block, and that must be as visible in the trail as a roster
edit is. A `workflow-changed-since-launch` audit entry now carries
`intake_running`/`intake_on_disk` beside `running`/`on_disk`, and fires on an
intake-only edit even when the pinned roster is untouched
(`intake_only_drift_is_audited_even_when_the_roster_is_unchanged`).

### A field can never be added to the intake schema unnoticed

The three `human_gate` denial tests (above) pin that a handful of specific
spellings are absent from `intake:`'s vocabulary. They do not, by themselves,
guard against a *new*, differently-named, gate-shaped field being added later
(`auto_merge:`, `skip_review:`, …) — that field would pass every existing
test, because nothing asserted the *set* of fields these types accept, only
that a few reserved spellings are missing from it.

`workflow.rs` closes that gap with a compile-time pin
(`intake_schema_field_inventory_is_exhaustively_named`): an exhaustive
struct-pattern destructure over `RawWorkflow`, `RawIntake` and
`RawIntakeLabels`, naming every field each type has today with no `..` to
swallow the rest. Rust's exhaustiveness rule turns a field added to any of
the three without being named in the matching destructure into a **compile
error**, not a silently passing test — forcing whoever adds the field to
look at this file and confirm, in the same PR, that it cannot weaken the
human gate before the inventory can be updated to accept it.

## The schema manifest: one statement, two enforcers (#880)

`src/workflow-schema.json` is a committed description of the wire format —
every section (workflow, block, edge, gate, intake, intake.labels, merge_queue,
resource) and every field, with its type, its closed value set where it has
one, its default, its bounds and the help text a form renders beside it.

It exists because of a failure mode that produced no error anywhere. `allow:`
was a `RawBlock` field from the day this schema shipped, and the workflow pane
never grew a control for it — nor even a *name* for it, so it fell into the
pane's unknown-key bag. A workflow that declared `allow:` therefore rendered,
in the GUI, exactly like a workflow that didn't. Nothing was wrong with the
engine and nothing was wrong with the pane; the two simply had no shared
statement of what a workflow file is, so they could not disagree out loud.
`intake:`, `merge_queue:` and `resources:` each arrived the same way later.

**The manifest is the schema rather than a description of it, in the specific
sense that tests hold each side to it — and it is worth being exact about which
parts those tests reach, because a row nothing checks is a row that drifts.**

- `src-tauri/tests/orchestration.rs` pins the **field names** of each section
  against the set serde actually accepts, derived from the `Raw*` types by
  *serializing* populated values (`workflow::workflow_schema_keys()`) rather
  than by hand-listing them. A hand-written list is precisely the thing that
  drifts, and it would drift silently in the one direction that matters: a new
  field, forgotten. Both directions fail: a `Raw*` field missing from the
  manifest is a field no GUI control will ever be generated for; a manifest
  field the engine doesn't have is a control that writes a key
  `deny_unknown_fields` rejects the whole file over.
- The same file pins the **values half** — each field's `values`, `default`,
  `min`, `max` and `max_entries` — against `workflow_schema_field_facts()`,
  which reads them off the engine's own accessors and constants
  (`SUPPORTED_CLIS`, `kind_names`, `role_hint_names`, `intake_source_names`,
  `builtin_intake_profile`, the `MergeQueuePolicy` / `ResourcePolicy` `Default`
  impls, `RESOURCE_*`, `NOTIFY_EXPIRES_*`, `RESOURCES_MAX`). Also both
  directions, so metadata cannot be invented here that the engine never stated.
  **Refuse-vs-clamp is pinned behaviorally**: the test drives `parse_workflow`
  with an out-of-range value and checks whether the file is refused or the
  value quietly pulled into range, because that is a fact about what the parse
  *does* and no constant states it.
- `test/workflowschema.test.ts` drives every manifest field through the pane's
  real parser and serializer: the parser must read it (never into `extra`), the
  canonical serializer must emit it, every enum value the pane has a rule for
  must pass that rule, and each field must be either claimed by a form control
  or explicitly listed as not yet having one.

Two enforcers and not one, deliberately. They pin different sides, and a repo
where only one is green is a repo where the pane and the engine disagree about
what a workflow file is — which is exactly the thing a human then gets lied to
about.

**What is deliberately not pinned**, so the claim above stays honest: `title`
and `help` prose; `gate.require`'s accepted set, which the engine states only
as match arms in `parse_workflow` and which is therefore hand-listed in
`workflow_schema_field_facts()` with that caveat attached; and `effort` /
`context`, which the pane has no rule of its own for because they are
capability data the backend answers per CLI and model (`agent_cli_knobs`).
Full engine parity for a live buffer remains `workflow_check`'s job, not the
manifest's.

**A field the pane can EDIT is a field the pane needs a rule for (#1020).**
When the inspector grew forms for `intake:`, `merge_queue:` and `resources:`,
each of those forms had to be able to answer "may I offer this value?" — and a
picker that can spell what `parse_workflow` refuses is the same lie as a pane
that blesses an illegal file, just earlier in the sequence. So `intake.source`,
the intake label alphabet, the resource-name alphabet and the four numeric
bounds joined the closed sets `workflowmodel.ts` already mirrors
(`WORKFLOW_CLIS`, `BLOCK_KINDS`, `GATE_REQUIRES`, `roleHintRequires`), for the
same reason and under the same discipline: hand-written because that module is
pure and import-free, and *pinned* — `test/workflowschema.test.ts` now checks
each constant against the manifest's own `min`/`max`/`max_entries`/`values`,
which the Rust side already checks against the engine's constants. Engine →
manifest → pane, with no step left to assumption. The manifest's
`on_out_of_range` is honored rather than merely read: a bound the engine
**refuses** is an error finding and a bound it **clamps** is a warning, because
"your file will not load" and "your file will not do what it says" send a human
to different places.

**A bound at its point of use is a bound nothing can check.** The first cut of
those forms clamped `merge_queue.max_batch` to a hand-typed `64` — a ceiling no
engine constant imposes and no manifest row declares — so typing `100` silently
wrote `64`, and no test in the tree could see it, because the pin asserted only
the bounds it happened to name. The fix is `POLICY_BOUNDS` in
`workflowmodel.ts`: one table, keyed by manifest field id, that every bounded
number in the pane reads, pinned against the manifest **in both directions** —
a table bound the manifest does not declare fails, and a manifest bound no form
reads fails too. `max` is compared *including its absence*, since a manifest row
without one is the statement that the engine imposes no ceiling. That reverse
direction immediately found `gate.threshold`, bounded in the manifest since #880
and hand-wired in `gateForm` since #222 with no clamp at all; it now reads the
same table, and its empty state means UNDECLARED (raising `gate-bad-threshold`)
rather than silently writing `1` the human never typed.

**Where the file has three states, the control has three.**
`merge_queue.enabled` is absent, `true`, or `false`, and absent and `false` mean
the same thing to the engine (`#[serde(default)]`) — which is exactly why the
pane must not convert between them behind the human's back. A checkbox cannot
hold three states: ticking then unticking one wrote `enabled: false` onto a file
that never carried the key, and the repair that always deletes on untick drops an
explicit `false` somebody wrote. So this one field is a three-way picker that
shows what the file says. It is the only place in the pane where the distinction
is visible, and it is the one form whose entire subject is what the file
declares.

**An empty value can be a real value.** `block.cli: ""` means "inherit the
group's CLI, whatever the launcher picks" and `intake.source: ""` means the
built-in source — both are what most files actually contain, so both are
members of their enum's `values` rather than an absence the manifest forgot to
mention. A generated `<select>` that cannot express them cannot express a legal
file. (The pane is stricter than the engine on exactly one of these: it still
asks a block for an explicit `cli:`. Stricter is the safe direction — it can
annoy, it cannot mislead someone into a file that will not load — but it is a
divergence, and it is written down here rather than discovered later.)

The pane's `KNOWN_*` sets stay hand-written and are *pinned* by that test rather
than read from the manifest at runtime. `workflowmodel.ts` is pure and
import-free by design (its one import is a type), and a data file it had to load
before it could open a workflow would be a second way for the pane to fail at
exactly the moment a human needs it to work. The test is the link between the
two, and it is cheaper than the coupling would be.

### Unknown keys: preserved *and* refused

The pane keeps keys it doesn't know (`extra`) so a file written by a newer
loomux survives a round-trip through an older pane instead of being quietly
stripped by it. That is still true, and it is still right. But preserving alone
was a half-truth: the engine is `deny_unknown_fields`, so one typo (`promt:`)
makes `parse_workflow` refuse the **whole** file — gates and all — down the loud
`workflow-invalid` path, while the pane cheerfully reported "valid". Preserving
and *warning* is the honest pair; dropping the key is destructive and ignoring
it is a lie.

`gates:` is exempt, and that is not an oversight: the engine reads it as
`BTreeMap<String, RawGate>`, so a gate loomux has no machinery for still parses
— it is simply never enforced. Reporting it would be the pane inventing a
refusal the engine never makes.

### Section order belongs to the file

`serializeWorkflowPreserving` emits each top-level section where the *document*
put it, appending only sections the file never declared. It used to emit a fixed
order (front, blocks, edges, gates), which was indistinguishable from
document order while those three were the only sections — and stopped being so
the moment `merge_queue:` became one, because this repo's own workflow file
writes it above `blocks:`. A fixed order would have relocated it, and the
comment block introducing it, on the first unrelated edit.

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
