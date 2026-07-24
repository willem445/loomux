---
name: author-loomux-workflow
description: When a human describes an agent-orchestration workflow in natural language for a repo that uses loomux (roles needed, review rigor, special personas, model/cost tiers), use this skill to author a correct `.loomux/workflow.yml` (+ persona files) against loomux's actual parser contract — never by pattern-matching another tool's YAML or guessing field names.
---

# Author a loomux `.loomux/workflow.yml`

This skill is for an agent working **inside a repo that loomux orchestrates**,
asked to turn a human's plain-language description of a workflow ("I want a
cheap worker tier, a strict security reviewer, and a database expert on call")
into a working `.loomux/workflow.yml` plus any persona files it references.

**Ground every schema claim in the parser, not in this document's prose.**
`src-tauri/src/orchestration/workflow.rs`'s `RawWorkflow`/`RawBlock`/`RawEdge`/`RawGate`
struct definitions and `parse_workflow` are the actual contract — this file is
a distillation of them as of the commit it was written against. If the repo
you're working in has a newer `workflow.rs`, the parser wins; re-derive the
field table below from it before authoring anything. The sibling
`agent-cli-reference` skill states the same rule for agent-CLI facts; this is
that discipline applied to loomux's own schema.

## Before you write anything: the one-line context check

`.loomux/workflow.yml` only does anything if the human turns on the
**advanced orchestrator** toggle for that repo (at launch, or live from the
group lifecycle panel) — off (the default), loomux never even opens the
file. Say this to the human once, in your summary: writing the file is not
enough on its own, they still need to flip that switch and look at the
resolved-roster preview loomux shows before anything spawns. That preview
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
  the security-critical review lane (e.g. `claude` / `opus`). Only `claude`
  and `copilot` exist as CLIs today (`SUPPORTED_CLIS`) — don't invent a
  third.
- **Review rigor.** One reviewer that must pass, or several focused lanes
  that must *all* pass (`all-pass`), or "any N of these M" (`threshold: N`)?
  This becomes the `gates.merge` clause (Step 4).
- **Special personas.** A domain expert consulted on demand (→ a
  `kind: planner` block with `role_hint: advisor` — read-only, spawned only
  when the orchestrator is stuck on a question), or a process/lessons role
  that runs after a merge (→ `kind: worker` with `role_hint: process`). Both
  hints are optional and purely cosmetic — see Invariant 3.
- **What stays default.** If the human didn't ask for something (a planner,
  a second worker tier, a merge gate at all), don't invent it. A workflow
  file that declares only what it's for is easier to read and easier for the
  human to consent to. Blocks the file doesn't declare simply don't exist —
  loomux doesn't backfill them (except the orchestrator; see Invariant 2).

## Step 2 — MAP to loomux concepts

| Human language | loomux concept |
|---|---|
| "a role" / "an agent that does X" | a **block**: `id` (immutable identity), `name` (display only), `kind` (capability class), `cli`, `model`, and a persona (`prompt:` or `profile:`) |
| "what kind of work can it do" | `kind` — one of exactly four: `orchestrator`, `worker`, `reviewer`, `planner`. This is the **only** thing that grants capability. See Invariant 1. |
| "cheap" / "strong" / "which model" | `cli:` + `model:` on the block. Empty `cli:` inherits the group's default CLI; empty `model:` inherits the kind's default for the resolved CLI (`opus` for orchestrator/planner, `sonnet` for worker/reviewer on `claude`; always `auto` on `copilot`). |
| "a domain expert, consulted on demand" | `kind: planner` + `role_hint: advisor` — read-only, spawned only when stuck on a specific question, exits the moment it reports. |
| "someone who writes up lessons after a PR merges" | `kind: worker` + `role_hint: process` — opens a normal PR, never merges it, same human gate as any worker. |
| "must all pass" / "any 2 of these 3" | `gates.merge.require: all-pass` (the default) or `require: threshold` + `threshold: N` |
| "also needs CI green" | `gates.merge.also: [ci-green]` — the only condition the shim can check today (see Step 5) |
| "the happy path" / "who hands off to whom" | `edges:` — **advisory only**. The orchestrator's scheduling judgment is the feature; edges are context it's shown, never a graph it's forced to walk. |

## Step 3 — the INVARIANTS (never express these in the file, ever)

These aren't style preferences — the parser enforces them, and a workflow
file that tries to spell any of them out is a **hard parse error**, not a
soft warning:

1. **A workflow file can never grant a capability.** `kind` selects one of
   four closed enum values; there is no `read_only: false`, no `allow_write`,
   no fifth class. `deny_unknown_fields` is on every wire struct, so a made-up
   key is a validation error, not a silent no-op. `allow:` can only
   *pre-approve tool patterns within what the kind already permits* — and is
   flatly **banned** on a read-only kind (`planner`), because a pre-approved
   shell pattern (`Bash(python *)`) could write files even though nothing on
   the deny list names it.
2. **The human merge gate is not expressible or removable in config.** A
   workflow's `gates.merge` is an *additional* necessary condition enforced
   by the `gh` PATH shim — it never substitutes for, weakens, or bypasses
   loomux's own default-branch human-approval gate. There is no field that
   turns that off.
3. **No agent ever merges a PR** — not a worker, not a `process`-hinted
   worker, not a reviewer. Every block opens a PR and stops; a human merges.
   This isn't configurable per block either.
4. **`role_hint` is cosmetic, never structural.** `advisor`/`process` select
   only a persona addendum, a template fragment, and a roster badge.
   Capability comes from `kind` alone, always — `kind_from_str` and
   `role_hint_requires` both *reject* unrecognized or mismatched values
   rather than coercing them, so you cannot spell a fifth capability class
   by combining hint + kind cleverly.
5. **The orchestrator block is loomux-owned.** A workflow file may pin its
   `cli:`/`model:` and nothing else — `prompt:`, `profile:`, and `allow:` on
   an `orchestrator`-kind block are a parse error. It is loomux's trust root;
   a repo-authored persona there would be a direct prompt-injection seam with
   no gate. Put personas on the blocks the orchestrator spawns, never on it.
6. **`mechanics_core` rides every persona non-overridably.** Even a `profile:`
   persona in `mode: replace` (which replaces the built-in role *body*)
   cannot strip the functional contract — the MCP tools, `report()`
   discipline, the task board, branch→PR flow, "never merge". Personas
   flavor an agent; they cannot re-arm what its `kind` denies or unbind what
   loomux always injects.

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

One block (`RawBlock`, `deny_unknown_fields`):

| Field | Type | Required | Notes |
|---|---|---|---|
| `id` | string | yes | immutable identity; `[A-Za-z0-9_-]` only, ≤48 chars, unique; the four kind names (`orchestrator`/`worker`/`reviewer`/`planner`) are **reserved** — usable only by a block of that same `kind` |
| `name` | string | no (default `""`) | display only; falls back to `id` if empty; renaming never breaks a reference |
| `kind` | string | yes | one of `orchestrator`, `worker`, `reviewer`, `planner` (case-insensitive); anything else is a named error, never coerced |
| `cli` | string | no (default `""`) | `""` = inherit the group default; else must be `claude` or `copilot` |
| `model` | string | no (default `""`) | `""` = inherit the kind's default for the resolved `cli`; allowlist-filtered (alnum, `.`, `-`, `_`) |
| `prompt` | string | no | inline persona text; mutually exclusive with `profile` |
| `profile` | string | no | repo-relative path to a persona file; mutually exclusive with `prompt`; no `..`, no absolute path, no drive letter |
| `allow` | list of string | no (default `[]`) | extra pre-approved tool patterns; **rejected outright** if the block's `kind` is read-only (`planner`) |
| `role_hint` | string | no | `advisor` (requires `kind: planner`) or `process` (requires `kind: worker`); any other value, or a value paired with the wrong `kind`, is a parse error |

Special case: a `kind: orchestrator` block may set only `cli`/`model` —
`prompt`, `profile`, or a non-empty `allow` on it is a parse error
(Invariant 5).

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
dogfood `.loomux/workflow.yml`, which runs two worker tiers and three
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

### Persona-file template (`.github/agents/<name>.md`)

Required frontmatter is a lenient `key: value` skim (`parse_profile` in
`profiles.rs`), **not** a strict YAML parser — it's Copilot's own custom-agent
file format, so copilot-native keys (`tools:`, `agents:`, …) are read by
Copilot itself and silently ignored by loomux. The keys loomux understands:

| Key | Required | Notes |
|---|---|---|
| `name` | no | defaults to the file stem; also the Copilot `--agent <name>` handle |
| `description` | no | one-line summary; supports YAML's `>` folded-scalar form |
| `kind` (or `role`) | no | a **compatibility check only** — if present, it must match the block's `kind` or loading the persona is an error. Never use it to move a block into a different class. |
| `mode` | no (default `append`) | `append` layers the persona on loomux's built-in role contract; `replace` swaps the role *body* but never `mechanics_core` (Invariant 6) |
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
Tauri command reachable only from inside the loomux app (the launcher's
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
   - an edge or a gate `reviewers:` entry naming a block id that doesn't
     exist, or a gate naming a block that exists but isn't `kind: reviewer`;
   - a `threshold:` greater than the number of named reviewers;
   - a duplicate block id, or the same reviewer named twice in one gate.
   `parse_workflow` reports **every** problem in one pass, not just the
   first — read the whole error list if you have one, don't fix-and-rerun
   one at a time.
2. **A broken or missing file never blocks a launch** — loomux audits and
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

## Step 6 — PITFALLS (from this repo's own history)

- **Comment-preserving YAML, but only through the pane (#233).** If a human
  later edits the file through loomux's GUI workflow pane, their comments
  survive (`serializeWorkflowPreserving` reuses the original text's own lines
  per top-level piece it didn't touch). This means it's safe to leave
  explanatory comments in the file you author — including per-block
  rationale, the way this repo's own dogfood `.loomux/workflow.yml` does —
  they won't get silently stripped on the next GUI save. They *will* be
  fully rewritten if the edit changes that same piece, so don't rely on a
  comment surviving an edit to the exact block/gate it's attached to.
- **Unknown-field rejection is strict by design, not an accident to work
  around.** Every wire struct in `workflow.rs` carries
  `#[serde(deny_unknown_fields)]` specifically so a typo'd key (`promt:`,
  `kinds:`, `revewers:`) is a loud parse error instead of the silent no-op
  every other workflow tool in the survey ships (Flowise/Langflow/Dify all
  discover a bad key at runtime, or never). Don't add speculative fields
  "in case loomux supports them later" — it doesn't, and the parser will
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
- **Persona text with apostrophes is fine — this has been true since the
  block model itself landed (#222), not from a later fix.** `sanitize_persona`
  maps `'` → the typographic `’` before the persona reaches a shell's
  single-quoted `--agents` payload, so `"don't"` in a `prompt:` reads fine
  and needs no special escaping from you. (Double-check this against the
  current `workflow.rs` if you're working in a much later commit — but as of
  this writing there is no separate "apostrophe quoting fix" era to worry
  about; don't invent one.)
- **Resume-pinning: a workflow change mid-group doesn't apply until
  relaunch.** Only a **fresh** launch reads `.loomux/workflow.yml` — a
  **resumed** group keeps running the roster (and gate) it was launched
  with, even if you've since edited the file, because a resume is not a
  consent moment (nobody's looking at a preview). loomux detects the drift
  and audits it (`workflow-changed-since-launch`) rather than silently
  applying it. If you author or edit a workflow file for a group that's
  already running, tell the human the change needs a relaunch (or the live
  advanced-orchestrator toggle flip, which *is* a consent moment) — it will
  not take effect on its own.

## What's coming, not yet merged

An `intake:` block (issue #382, PR #403) is on the review gate as of this
writing — do not document its fields or assume it's part of the schema
above until it's actually merged to `main`; re-check `workflow.rs` for it
before relying on anything beyond what's in the schema table.
