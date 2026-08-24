---
name: author-loomux-workflow
description: When a human describes an agent-orchestration workflow in natural language for a repo that uses orrerix (roles needed, review rigor, special personas, model/cost tiers), use this skill to author a correct `.orrerix/workflow.yml` (+ persona files) against orrerix's actual parser contract — never by pattern-matching another tool's YAML or guessing field names.
---

# Author an orrerix `.orrerix/workflow.yml`

This skill is for an agent working **inside a repo that orrerix orchestrates**,
asked to turn a human's plain-language description of a workflow ("I want a
cheap worker tier, a strict security reviewer, and a database expert on call")
into a working `.orrerix/workflow.yml` plus any persona files it references.

**Ground every schema claim in the parser, not in this document's prose.**
`crates/loomux-engine/src/workflow.rs`'s `RawWorkflow`/`RawBlock`/`RawEdge`/`RawGate`
struct definitions and `parse_workflow` are the actual contract — this file is
a distillation of them as of the commit it was written against. If the repo
you're working in has a newer `workflow.rs`, the parser wins; re-derive the
field table below from it before authoring anything. The sibling
`agent-cli-reference` skill states the same rule for agent-CLI facts; this is
that discipline applied to orrerix's own schema.

## Before you write anything: the one-line context check

`.orrerix/workflow.yml` only does anything if the human turns on the
**advanced orchestrator** toggle for that repo (at launch, or live from the
group lifecycle panel) — off (the default), orrerix never even opens the
file. Say this to the human once, in your summary: writing the file is not
enough on its own, they still need to flip that switch and look at the
resolved-roster preview orrerix shows before anything spawns. That preview
*is* the toggle's consent moment — don't present the file as something that
silently takes effect.

## Step 1 — ELICIT

Read the human's description and extract, explicitly, before writing YAML:

- **Roles needed.** Who plans, who builds, who reviews? A role that isn't
  named gets no block — `spawn_agent(kind: X)` against a roster with no `X`
  block fails outright rather than guessing (this is deliberate: see
  Invariant 2 below).
- **Tiers within a role.** "A cheap worker for small stuff and a strong one
  for anything with judgment in it" is two `kind: worker` blocks, not one.
  Nothing caps how many blocks share a `kind`.
- **Models/CLIs per role — the cost/capability tradeoff.** Cheaper/faster
  model+CLI combos for high-volume or mechanical work (e.g. `copilot` /
  `auto`, or `claude` / `haiku`); stronger ones for judgment-heavy work or
  the security-critical review lane (e.g. `claude` / `opus`). Only `claude`,
  `copilot`, `gemini` and `opencode` exist as CLIs today (`SUPPORTED_CLIS`) —
  don't invent a fifth. `gemini` and `opencode` are the cross-model options
  (#267, #722): a different model family for a reviewer lane. `gemini` defaults
  to its `pro` tier; `opencode` has NO default at all (its ids are
  `provider_id/model_id` against dozens of providers, so orrerix picks none and
  the pane inherits the human's own config) — an opencode block that wants a
  specific model must pin the FULL id, provider half included. Neither can host a
  block whose class it can't contain — that pairing is a parse error, not a
  warning — and `allow:` patterns apply to neither: those are Claude/Copilot
  tool-matcher strings, and a gemini or opencode block runs with its class's
  baseline.
- **A pane the HUMAN talks to.** "I want to chat about the project rather than
  drive the orchestrator", "somebody to take my half-formed feature ideas and
  turn them into a proper ticket", "status without reading agent traffic" → a
  `kind: manager` block. It is a capability class of its own, not a persona on a
  reviewer: no fleet traffic is ever delivered into that pane (no notices, no
  relays, no status lines), and the only two things orrerix itself writes there
  are the kickoff and — after a mid-session compact — one re-grounding notice.
  The orchestrator reaches it only by posting to a durable mailbox it pulls, and
  it holds no authority the human has not used themselves — no spawns and no
  verdicts (structural), and no branches or PRs (its containment denies the
  editing tools; the designed path has the orchestrator file the issue).
  **At most one per file**, and a second
  is a parse error. Declare one only if the human asked for that shape; a group
  without one behaves exactly as it does today. See `docs/features/manager.md`.
- **Review rigor.** One reviewer that must pass, or several focused lanes
  that must *all* pass (`all-pass`), or "any N of these M" (`threshold: N`)?
  This becomes the `gates.merge` clause (Step 4).
- **Special personas.** A domain expert consulted on demand (→ a
  `kind: planner` block with `role_hint: advisor` — read-only, spawned only
  when the orchestrator is stuck on a question), or a process/lessons role
  that runs after a merge (→ `kind: worker` with `role_hint: process`). Both
  hints are optional; neither is purely cosmetic — a hint can change which MCP
  tools its block is offered, within the enumerated list. See Invariant 4.
- **A design-review or premortem "second lens".** Same shape as the
  domain-expert advisor above (`kind: planner` + `role_hint: advisor`) —
  never `kind: reviewer`: orrerix's own built-in orchestrator template runs
  every reviewing block (every `kind: reviewer` block except one hinted
  `role_hint: liaison`) on every PR, so an on-demand lens declared as a
  reviewer runs on every PR regardless (defeating "on demand"), and naming it
  in a merge gate on top of that holds every merge shut until someone spawns
  it and it passes. See `docs/orchestration.md` → "Adding a second lens" for
  two ready-made personas (`design-review.md`, `premortem.md`) — including
  the caveat that `role_hint: advisor` gives you the shape, not an automatic
  trigger for *when* to spawn one.
- **A mechanical checklist lane that SHOULD run on every PR.** The mirror of the
  bullet above, and the one case where `kind: reviewer` in the gate is right:
  a cheap lane on a small/fast model that runs a fixed checklist of shell
  commands (evidence present? run id at the head? forbidden import?) ahead of
  the strong reviewer. The deciding question is never how important the opinion
  is — it is **does this run on every PR?** On demand → `planner` + `advisor`;
  every PR → `reviewer`, in the gate. See `docs/orchestration.md` → "Cheap
  lanes ahead of the expensive one".
- **What stays default.** If the human didn't ask for something (a planner,
  a second worker tier, a merge gate at all), don't invent it. A workflow
  file that declares only what it's for is easier to read and easier for the
  human to consent to. Blocks the file doesn't declare simply don't exist —
  orrerix doesn't backfill them (except the orchestrator; see Invariant 2).

## Step 2 — MAP to orrerix concepts

| Human language | orrerix concept |
|---|---|
| "a role" / "an agent that does X" | a **block**: `id` (immutable identity), `name` (display only), `kind` (capability class), `cli`, `model`, and a persona (`prompt:` or `profile:`) |
| "what kind of work can it do" | `kind` — one of exactly five: `orchestrator`, `worker`, `reviewer`, `planner`, `manager`. This is the **only** thing that grants capability. See Invariant 1. |
| "cheap" / "strong" / "which model" | `cli:` + `model:` on the block. Empty `cli:` inherits the group's default CLI; empty `model:` inherits the kind's default for the resolved CLI (`opus` for orchestrator/planner/**manager** — the reasoning-heavy classes — and `sonnet` for worker/reviewer on `claude`; always `auto` on `copilot`; always `pro` on `gemini`). |
| "a domain expert, consulted on demand" | `kind: planner` + `role_hint: advisor` — read-only, spawned only when stuck on a specific question, exits the moment it reports. |
| "a design-review or premortem second opinion, not on every PR" | Same `kind: planner` + `role_hint: advisor` shape — never `kind: reviewer`, which the merge gate would need to pass on every PR or run on every PR to avoid holding it shut. See `docs/orchestration.md` → "Adding a second lens". |
| "a pane I talk to" / "turn my idea into a ticket" / "status as a conversation" | `kind: manager` — the human's own interface. At most one per file (a second is a parse error); never spawned by an agent and never typed into; reaches the orchestrator through a durable mailbox and `message_orchestrator`. Not `kind: reviewer` + `role_hint: liaison`, which is the superseded shape. |
| "someone who writes up lessons after a PR merges" | `kind: worker` + `role_hint: process` — opens a normal PR and never merges it. Its PRs are a standing-authorized merge class the orchestrator dispositions itself rather than deferring to the human (#1021); the bar (review, green CI, findings settled) is unchanged. |
| "must all pass" / "any 2 of these 3" | `gates.merge.require: all-pass` (the default) or `require: threshold` + `threshold: N` |
| "also needs CI green" | `gates.merge.also: [ci-green]` — the only condition the shim can check today (see Step 5) |
| "the happy path" / "who hands off to whom" | `edges:` — **advisory only**. The orchestrator's scheduling judgment is the feature; edges are context it's shown, never a graph it's forced to walk. |

## Step 3 — the INVARIANTS (never express these in the file, ever)

These aren't style preferences — the parser enforces them, and a workflow
file that tries to spell any of them out is a **hard parse error**, not a
soft warning:

1. **A workflow file can never grant a capability.** `kind` selects one of
   five closed enum values; there is no `read_only: false`, no `allow_write`,
   no sixth class. `deny_unknown_fields` is on every wire struct, so a made-up
   key is a validation error, not a silent no-op. `allow:` can only
   *pre-approve tool patterns within what the kind already permits* — and is
   flatly **banned** on a read-only kind (`planner`), because a pre-approved
   shell pattern (`Bash(python *)`) could write files even though nothing on
   the deny list names it.
2. **The human merge gate is not expressible or removable in config.** A
   workflow's `gates.merge` is an *additional* necessary condition enforced
   by the `gh` PATH shim — it never substitutes for, weakens, or bypasses
   orrerix's own default-branch human-approval gate. There is no field that
   turns that off. A `role_hint` does not turn it off either: a hint only
   *selects* from orrerix's closed set, and orrerix's own code fixes what the
   selection means, so config can opt into a behaviour orrerix defines but can
   never author one.
3. **No delegate block ever merges a PR** — not a worker, not a
   `process`-hinted worker, not a reviewer. Every one of them opens a PR and
   stops, and this isn't configurable per block. The orchestrator merges only
   where a gate opened for it — autonomous auto-merge, a one-time human
   grant, supervised dangerous mode, or a standing class authorization
   (process-pro PRs are one, #1021). Absent a gate, a human merges.
4. **You cannot author what a `role_hint` means.** It mostly selects a persona
   addendum, a template fragment, and a roster badge. Capability comes from
   `kind` — `kind_from_str` and `role_hint_requires` both *reject* unrecognized
   or mismatched values rather than coercing them, so you cannot spell a fifth
   capability class by combining hint + kind cleverly. A few MCP tools do read
   the hint, and the rules do not all point the same way: two NARROW the class
   the hint sits on (`session_digest` offered to `process`-hinted workers alone;
   `review_verdict` withheld from a `liaison`-hinted reviewer) and two WIDEN it
   (`group_usage` and `ask_human`, both otherwise orchestrator-only, offered to
   that same liaison; `withdraw_question` is not).
   Every exception is enumerated in `doc/design/liaison.md`. What you cannot do
   from a workflow file is invent one: you pick from a closed set and orrerix's
   code decides the effect.
5. **The orchestrator block is orrerix-owned.** A workflow file may pin its
   `cli:`/`model:`/`effort:`/`context:` and nothing else — `prompt:`,
   `profile:`, and `allow:` on an `orchestrator`-kind block are a parse
   error. The pin list is exactly the picks from a closed value set orrerix
   already ships (no field there authors text or pre-approves a tool), which
   is why `effort:`/`context:` joined it. It is orrerix's trust root; a
   repo-authored persona there would be a direct prompt-injection seam with
   no gate. Put personas on the blocks the orchestrator spawns, never on it.
6. **`mechanics_core` rides every persona non-overridably.** Even a `profile:`
   persona in `mode: replace` (which replaces the built-in role *body*)
   cannot strip the functional contract — the MCP tools, `report()`
   discipline, the task board, branch→PR flow, "never merge". Personas
   flavor an agent; they cannot re-arm what its `kind` denies or unbind what
   orrerix always injects.

## Step 4 — AUTHOR

### Schema reference (from `RawWorkflow`/`RawBlock`/`RawEdge`/`RawGate`, `workflow.rs`)

Top level (`RawWorkflow`, `deny_unknown_fields`):

| Field | Type | Required | Notes |
|---|---|---|---|
| `version` | int | yes | must equal `1` (`SCHEMA_VERSION`) — anything else is a parse error |
| `name` | string | no (default `""`) | display only |
| `authored_with` | string | no | purely informational stamp (e.g. `"loomux 0.8.0"`); **never** a validation error whatever it says |
| `blocks` | list of block | no (default `[]`) | at least one block, or `"no blocks declared"` |
| `edges` | list of edge | no (default `[]`) | advisory only |
| `gates` | map\<string, gate\> | no (default `{}`) | only the `merge` key is read by the `gh` shim today |
| `intake` | map | no (default: built-in profile) | where autonomous work comes from and what its label vocabulary is: `source` (`github-labels` (default), `board`, `none` — `board`/`none` are schema-reserved, not yet wired) and `labels` (`ready`, `investigate`, `owned`, `prototype`, `hold`; each independently overridable — `hold` names the human-veto label a full-autonomy group must never start, default `agent-hold`). It can never grant a capability, and there is no spelling of it that disables the human merge gate |
| `merge_queue` | map | no (default: disabled) | `enabled` (bool), `max_batch`, `checks_timeout_minutes`. Absent means disabled |

One block (`RawBlock`, `deny_unknown_fields`):

| Field | Type | Required | Notes |
|---|---|---|---|
| `id` | string | yes | immutable identity; `[A-Za-z0-9_-]` only, ≤48 chars, unique; the five kind names (`orchestrator`/`worker`/`reviewer`/`planner`/`manager`) are **reserved** — usable only by a block of that same `kind` |
| `name` | string | no (default `""`) | display only; falls back to `id` if empty; renaming never breaks a reference |
| `kind` | string | yes | one of `orchestrator`, `worker`, `reviewer`, `planner`, `manager` (case-insensitive); anything else is a named error, never coerced. `manager` is capped at **one per file** and may not be named as a gate reviewer (it records no verdict); `prompt:`/`profile:`/`allow:` on a manager block are parse errors, as they are on an orchestrator block |
| `cli` | string | no (default `""`) | `""` = inherit the group default; else must be one of `SUPPORTED_CLIS` (`claude`, `copilot`, `gemini`) |
| `model` | string | no (default `""`) | `""` = inherit the kind's default for the resolved `cli`; allowlist-filtered (alnum, `.`, `-`, `_`) |
| `prompt` | string | no | inline persona text; mutually exclusive with `profile` |
| `profile` | string | no | repo-relative path to a persona file; mutually exclusive with `prompt`; no `..`, no absolute path, no drive letter |
| `allow` | list of string | no (default `[]`) | extra pre-approved tool patterns; **rejected outright** if the block's `kind` is read-only (`planner`) |
| `role_hint` | string | no | `advisor` (requires `kind: planner`), `process` (requires `kind: worker`), or `liaison` (requires `kind: reviewer`); any other value, or a value paired with the wrong `kind`, is a parse error. **`liaison` is superseded by `kind: manager`** — it still parses and still runs, and the workflow pane warns on it; write `kind: manager` in a new file |
| `effort` | string | no (default `""`) | thinking level; `""` = the CLI's own default. One of `low`, `medium`, `high`, `xhigh`, `max` — see the caps-gating rule below |
| `context` | string | no (default `""`) | context-window variant; `""` = the model's own window. One of `1m` today — same caps-gating rule. Composed into the model alias at emit (`sonnet[1m]`), never written into `model:` itself |

**The caps-gating rule for `effort`/`context` (#687):** each is checked
twice at parse time, and either check failing is a parse error, never a
silent drop. First, the value must be in orrerix's own closed vocabulary
above (a typo is never coerced to a neighboring level). Second, the block's
own `cli:` must be a CLI orrerix can actually deliver that knob on — today
that's `claude` for both knobs; `copilot` and `gemini` accept neither
(copilot's effort is settings-file-only with no flag/env, its context
window is interactive-only; gemini's thinking level is a settings-file seam
that exists but is unwired pending live schema verification). Declaring
`effort:`/`context:` on a block whose `cli:` can't honor it is a parse error
naming the CLI and why, not a value that silently does nothing.

**Which CLI carries which knob — check before you write either key.** The
launcher greys an undeliverable knob out for a human; authoring the file by
hand there is no such rail, so this is it:

| `cli:` | `effort:` | `context:` | why |
|---|---|---|---|
| `claude` | `low`, `medium`, `high`, `xhigh`, `max` | `1m` | `--effort <level>` is a session flag; `[1m]` is a model-alias suffix |
| `copilot` | **none** | **none** | effort is `~/.copilot/settings.json`-only (no flag, no env, and orrerix never writes a user's global settings); the context window is the interactive `/context` control |
| `gemini` | **none** | **none** | its thinking level is a settings-file key whose schema is unverified, so orrerix does not write it; its window is model-determined |
| `opencode` | **none** | **none** | its reasoning effort is a model *variant* — a flag on `opencode run`, absent from the TUI orrerix spawns — and its context window is model-determined |

**This table is a snapshot; `CliCaps` is the truth.** The rows come from
`CLI_CAPS` in `crates/loomux-engine/src/model.rs`, which the `agent_cli_knobs`
Tauri command serves to the launcher — same source, so the launcher's greyed-out
select and `parse_workflow`'s refusal can never disagree. A knob gets wired on a
new CLI by adding values to that row, and this table is then stale. If the two
disagree, `CLI_CAPS` wins and this table is the bug. **An empty cell is a
positive claim, not a gap** — it means orrerix has looked for a seam and found
none it can use, which is why the parse error can quote a vendor reason rather
than saying "unsupported".

If you get the refusal anyway, read it: it names the block (index and id), the
knob, the exact value, the vendor reason, and both fixes — drop the key, or move
the block to a `cli:` that carries it. Dropping the key is not a downgrade; it
means the CLI's own default applies.

**A third check exists for `context:`, but it is not in `parse_workflow` —
a clean parse is necessary, not sufficient.** The `[1m]` suffix also has to
fit the block's `model:`: it's only defined for the `sonnet`/`opus`/
`opusplan` families, not `haiku`/`fable`/`best`/`default` — and it fails
open (stays valid) on a model id orrerix doesn't recognize, e.g. a
Bedrock/Vertex/Foundry deployment name. `parse_workflow` does not check this
at all: `model: haiku` with `context: 1m` parses cleanly. The workflow
**pane** does check it — it raises a `knob-unavailable` finding naming the
same reason the launcher's own select would grey out for — and the CLI
itself rejects `haiku[1m]` at spawn if such a file is ever launched without
the pane's finding having been seen. Don't call a `context:` value valid off
`parse_workflow` alone; open the file in the workflow pane, or reason
through the family rule by hand, first.

Special case: a `kind: orchestrator` block may set only
`cli`/`model`/`effort`/`context` — `prompt`, `profile`, or a non-empty
`allow` on it is a parse error (Invariant 5).

One edge (`RawEdge`, `deny_unknown_fields`):

| Field | Type | Required | Notes |
|---|---|---|---|
| `from` | string | yes | must name a declared block |
| `to` | string or list of string | yes | each entry must name a declared block; `to: worker` and `to: [a, b]` both parse |

One gate, keyed by name in the `gates:` map (`RawGate`, `deny_unknown_fields`)
— only `merge` does anything today:

| Field | Type | Required | Notes |
|---|---|---|---|
| `require` | string | no | `"all-pass"` (default) or `"threshold"` |
| `threshold` | int | required iff `require: threshold` | must be `> 0` and `≤` the number of named `reviewers` |
| `reviewers` | list of string | yes, non-empty | each must name a declared block whose `kind` is `reviewer`; no duplicates |
| `also` | list of string | no (default `[]`) | extra condition names; only `ci-green` is currently checkable (see Step 5's Pitfalls) |

### A complete worked example

A small team: one cheap worker tier, one strong reviewer, and a
domain-expert advisor spawned on demand. (Distinct from this repo's own
dogfood `.orrerix/workflow.yml`, which runs two worker tiers and three
lane-scoped reviewers — that file is worth reading as a second, larger
example, but don't copy its `all-pass`-over-three-lanes shape onto a team
that only asked for one reviewer.)

```yaml
version: 1
name: small-team

blocks:
  # The trust root. Only cli:/model: may be pinned here — see Invariant 5.
  - id: orchestrator
    kind: orchestrator
    cli: claude
    model: opus

  # The cheap tier: fast CLI, auto model, native Copilot persona file.
  - id: worker
    name: Worker
    kind: worker
    cli: copilot
    model: auto
    profile: .github/agents/worker.md

  # The strong reviewer: everything must pass this one lane.
  - id: rev-lead
    name: Lead reviewer
    kind: reviewer
    cli: claude
    model: opus
    prompt: |
      Review every PR for correctness and security. Reproduce findings before
      reporting them; block on anything you can't defend with a repro.

  # A domain expert, spawned only when the orchestrator is stuck on a
  # database question — read-only, exits the moment it reports.
  - id: db-advisor
    name: Database advisor
    kind: planner
    cli: claude
    model: opus
    profile: .github/agents/db-advisor.md
    role_hint: advisor

edges:
  - { from: orchestrator, to: [worker, db-advisor] }
  - { from: worker, to: [rev-lead] }

gates:
  merge:
    require: all-pass
    reviewers: [rev-lead]
    also: [ci-green]
```

### Worked example — a manager between the human and the fleet

Declare a `kind: manager` block when the human wants to talk to a *person*
rather than drive an orchestrator: project discussion, status as a
conversation, and turning a half-formed idea into a brief the team can build.

```yaml
version: 1
name: with-a-manager

blocks:
  - id: orchestrator
    kind: orchestrator
    cli: claude
    model: opus

  # The human's own interface. At most one per file; a second is a parse error.
  # No prompt:/profile:/allow: here — a manager block takes none, exactly as an
  # orchestrator block takes none (Invariant 5). Only cli: and model:.
  - id: manager
    name: Manager
    kind: manager
    cli: claude
    model: opus

  - id: worker
    kind: worker
    cli: claude
    model: sonnet

  - id: rev-lead
    kind: reviewer
    cli: claude
    model: opus

edges:
  - { from: orchestrator, to: [worker] }
  - { from: worker, to: [rev-lead] }

gates:
  merge:
    require: all-pass
    reviewers: [rev-lead]      # never the manager: it records no verdict
```

What changes for the human, and what does not:

- The manager pane opens with the group and is **never typed into** by orrerix.
  It learns what happened by reading a durable mailbox the orchestrator posts
  to, at the start of each of its turns — which is the next time its human
  speaks to it.
- It holds **no authority the human has not used themselves**: no repo writes,
  no spawns, no kills, no verdicts. It relays; the orchestrator decides.
- It does **not** start work. A brief it grooms becomes a GitHub issue, and the
  human's own label is still the only thing that hands that issue to the fleet.
- The manager is exempt from `max_agents` and from the idle reaper, so it does
  not compete with delegates for a slot and does not get closed for being quiet.
- The `edges:` list deliberately does not mention it. Edges describe the work
  handoff; the manager is not in that path.

Do not declare one the human did not ask for. A group without a manager behaves
exactly as it does today, and adding one changes where the human stands.


### Persona-file template (`.github/agents/<name>.md`)

Required frontmatter is a lenient `key: value` skim (`parse_profile` in
`profiles.rs`), **not** a strict YAML parser — it's Copilot's own custom-agent
file format, so copilot-native keys (`tools:`, `agents:`, …) are read by
Copilot itself and silently ignored by orrerix. The keys orrerix understands:

| Key | Required | Notes |
|---|---|---|
| `name` | no | defaults to the file stem; also the Copilot `--agent <name>` handle |
| `description` | no | one-line summary; supports YAML's `>` folded-scalar form |
| `kind` (or `role`) | no | a **compatibility check only** — if present, it must match the block's `kind` or loading the persona is an error. Never use it to move a block into a different class. |
| `mode` | no (default `append`) | `append` layers the persona on orrerix's built-in role contract; `replace` swaps the role *body* but never `mechanics_core` (Invariant 6) |
| `allow` | no | comma-separated extra tool patterns; same read-only-kind ban as the block's `allow:` |

Template, matching this repo's own `.github/agents/*.md` shape:

```markdown
---
name: db-advisor
description: >
  A read-only advisor on schema and query-performance questions, consulted
  on demand when the team is stuck. Investigates and reports; never merges,
  spawns, or edits.
kind: planner
mode: replace
---
You are consulted only when the team is stuck on a database question. The
orchestrator spawns you with a specific question and enough context to
investigate it.

## What you do

1. Investigate READ-ONLY: read the schema, migrations, and relevant queries.
   You cannot write a file, branch, or push — the planner capability class
   denies those at the CLI level regardless.
2. Answer the question you were asked. If it's under-specified, say so.
3. `report("done", "<your advice>")` — lead with the recommendation, then the
   reasoning, then anything you're not sure of.

## What you never do

No authority beyond advice: never merge, never spawn another agent, never
edit or push. The orchestrator decides what to do with your advice.
```

`mode: replace` is what most on-demand advisor/domain-expert personas want —
their whole point is a narrow, non-default persona. A worker/reviewer
persona that's mostly "focus on this lane" on top of the standard flow
usually wants the default `append` instead (omit `mode:` entirely — see
`worker-deep.md`/`rev-orch.md` in this repo for that shape).

## Step 5 — VALIDATE

There is **no standalone CLI validator** — `orch_workflow_preview` is a
Tauri command reachable only from inside the orrerix app (the launcher's
resolved-roster preview, or the workflow pane's live TypeScript-side check).
As an authoring agent you cannot invoke either directly, so:

1. **Validate by hand against the schema table above and `parse_workflow`'s
   rules**, not by assuming a well-formed-looking file is correct. Common
   things it rejects that look plausible:
   - an unrecognized top-level or block key (`deny_unknown_fields` — a typo
     like `promt:` is a hard error, not a silent no-op);
   - `kind: revieweer` or any other misspelled/unknown kind (never coerced
     to `worker`);
   - `prompt:` and `profile:` both set on the same block;
   - `allow:` on a `kind: planner` block;
   - `prompt:`/`profile:`/`allow:` on the `kind: orchestrator` block;
   - a `role_hint` on the wrong `kind` (`role_hint: advisor` on a `worker`
     block, etc.);
   - `effort:`/`context:` not in orrerix's closed vocabulary, or set on a
     block whose `cli:` can't honor that knob (e.g. `context: 1m` on a
     `copilot` block) — check the per-CLI knob matrix in Step 4 BEFORE
     writing either key; the refusal names both fixes if you don't;
   - `context: 1m` paired with a `model:` that has no `[1m]` form (`haiku`,
     `fable`, `best`, `default`) — `parse_workflow` does not check this (it
     checks the CLI, not the model), so this combination parses clean and
     still won't work at spawn; only the workflow pane's `knob-unavailable`
     finding catches it — see the third check in Step 4;
   - an edge or a gate `reviewers:` entry naming a block id that doesn't
     exist, or a gate naming a block that exists but isn't `kind: reviewer`;
   - a `threshold:` greater than the number of named reviewers;
   - a duplicate block id, or the same reviewer named twice in one gate.
   `parse_workflow` reports **every** problem in one pass, not just the
   first — read the whole error list if you have one, don't fix-and-rerun
   one at a time.
2. **A broken or missing file never blocks a launch** — orrerix audits and
   skips it, falling back to the built-in four-block roster. That is a
   safety property, not a substitute for getting the file right: it means
   the human's *custom* roster and merge gate silently don't apply, with
   only an audit-log line to explain why. Don't treat "the group still
   launched" as "the file is fine."
3. **Tell the human to check the resolved preview before trusting the
   file.** Once you've hand-validated, say so explicitly and point them at
   turning the advanced-orchestrator toggle on (or off-then-on if it's
   already on) — that's what actually runs `parse_workflow` and shows the
   resolved roster/errors as warnings before anything spawns.

## Step 6 — PITFALLS

- **Comment-preserving YAML, but only through the pane (#233).** If a human
  later edits the file through orrerix's GUI workflow pane, their comments
  survive (`serializeWorkflowPreserving` reuses the original text's own lines
  per top-level piece it didn't touch). This means it's safe to leave
  explanatory comments in the file you author — including per-block
  rationale, the way this repo's own dogfood `.orrerix/workflow.yml` does —
  they won't get silently stripped on the next GUI save. They *will* be
  fully rewritten if the edit changes that same piece, so don't rely on a
  comment surviving an edit to the exact block/gate it's attached to.
- **Unknown-field rejection is strict by design, not an accident to work
  around.** Every wire struct in `workflow.rs` carries
  `#[serde(deny_unknown_fields)]` specifically so a typo'd key (`promt:`,
  `kinds:`, `revewers:`) is a loud parse error instead of a silent no-op
  discovered at runtime, or never. Don't add speculative fields
  "in case orrerix supports them later" — it doesn't, and the parser will
  say so.
- **An `also:` condition the shim can't check fails the gate closed, not
  silently.** `also: [some-condition]` where `some-condition` isn't
  `ci-green` (the only entry in `KNOWN_CONDITIONS` today) **parses
  successfully** — the parser only sanitizes the character set, it doesn't
  check the name is known — but the gate can then never be satisfied,
  because the shim refuses on anything it doesn't recognize rather than
  ignoring it. If a human describes a condition that isn't CI status, don't
  silently drop it into `also:` and call it done — say plainly that it
  isn't enforceable today.
- **Persona text with apostrophes needs no escaping from you (#222).**
  `sanitize_persona` maps `'` → the typographic `’` before the persona
  reaches a shell's single-quoted `--agents` payload, so `"don't"` in a
  `prompt:` reads fine as written.
- **Resume-pinning: a workflow change mid-group doesn't apply until
  relaunch.** Only a **fresh** launch reads `.orrerix/workflow.yml` — a
  **resumed** group keeps running the roster (and gate) it was launched
  with, even if you've since edited the file, because a resume is not a
  consent moment (nobody's looking at a preview). orrerix detects the drift
  and audits it (`workflow-changed-since-launch`) rather than silently
  applying it. If you author or edit a workflow file for a group that's
  already running, tell the human the change needs a relaunch (or the live
  advanced-orchestrator toggle flip, which *is* a consent moment) — it will
  not take effect on its own.
