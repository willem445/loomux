# Supervisor/advisor agent + skills feedback loop

Issues #250 (advisor/supervisor) and #324 (skills feedback loop), converged
into one aggregate design on `feat/250-324-supervisor-skills`. This note
covers the foundation slice (A, the `role_hint` field) and the persona/
template slice (C) that render off it. The remaining slices (session digest,
process-pro wiring, the optional push-nudge) extend this note in place as
they land; see the plan comment on issue #250 for the full breakdown.

## The decision: new *blocks*, not new *Roles*

Both features want "a distinct agent type" — the human, on both issues.
Neither needs a new **capability class**:

- **Advisor** (#250) is read-only + can message the orchestrator — exactly
  `Role::Planner`'s posture (`is_read_only()` is `matches!(self,
  Role::Planner)`; a planner already has `get_state`/`message_orchestrator`
  and auto-frees its delegate slot on `report("done")`, #203).
- **Process-pro** (#324) reads the record and opens a PR — `Role::Worker`'s
  posture (worktree, git, `gh pr create` through the shim, `report`, board;
  cannot merge — human gate + shim).

Per #222's own thesis (`doc/design/workflows.md`: identity is data =
`BlockId`, capability is the enum = `Role`), a distinct agent type is a
distinct **block**, not a distinct **kind**. A new `Role::Advisor` /
`Role::ProcessPro` variant would touch every trust-reviewed capability-core
site (`enum Role`, `prefix`/`template`/`instructions_file`/`as_str`/
`is_read_only`/`mechanics_core`/`default_model`, the MCP tool scope,
`kind_from_str`/`kind_names`, the TS `BLOCK_KINDS` mirror, CSS chip classes, a
new template) and re-bless every test that pins "exactly four values" — for
zero new capability. **Rejected.**

## The marker: `Block.role_hint`

A new *optional* `role_hint: Option<String>` field on `Block`
(`src-tauri/src/orchestration/workflow.rs`), values `advisor` | `process`,
**inert with respect to capability**. It is validated at parse time to
*require* its matching class — `advisor` needs `kind: planner`, `process`
needs `kind: worker` — and an unrecognized value or a mismatched pairing is a
loud, named parse error, never a silent fallback or a coerced kind (the same
shape `kind_from_str` already enforces for `kind` itself).

```yaml
blocks:
  - id: advisor
    kind: planner
    role_hint: advisor    # requires kind: planner
  - id: process
    kind: worker
    role_hint: process     # requires kind: worker
```

`role_hint` drives **only** persona/template/badge selection — which
`.github/agents/*.md` addendum a block's mechanics core gets, which template
fragment renders in `templates/orchestrator.md`/`worker.md`, and which
ADVISOR/PROCESS chip the launcher preview and roster show. Capability and
trust continue to key **exclusively** off `kind`: `Role::is_read_only()`,
`mcp::tool_defs(Role)`, and the CLI-level deny-flags never see `role_hint` at
all — the functions that decide them take a `Role`, not a `Block`, and adding
the field did not change a single one of their call sites. A workflow file
still cannot grant a capability; `role_hint` can only *select a kind that
already grants nothing new*.

**Persistence.** `role_hint` round-trips through both wire formats: parsed
`.loomux/workflow.yml` (`parse_workflow`) and the persisted `group.json`
roster (`blocks_json`/`read_blocks`). The `group.json` path applies the same
defense-in-depth the `kind` field already gets — a hand-edited file that
never met the parser has its role_hint silently dropped (not resurrected) if
it no longer matches the block's own `kind`, since there is no human at that
layer to show a parse error to.

**Alternative considered:** reserving the ids `advisor`/`process` themselves
(extending the `BUILTIN_IDS`/reserved-id-per-class mechanism already used for
the four built-in roles) — cheaper (no new wire field) but *implicit*.
`role_hint` is explicit and self-documents in the file the human consents to
at launch time; the launcher preview surfaces it as its own field precisely
so the consent moment can name it.

## What this buys, for free

- **The advisor cannot acquire authority.** Planner-kind means
  `is_read_only()` denies Edit/Write/git at the CLI level, mechanically —
  not instruction-backed. An advisor block interjects advice; it can never
  merge, spawn, or record a verdict, whatever its persona says.
- **The process-pro proposes, never disposes.** Worker-kind means the `gh`
  shim refuses `gh pr merge` from that pane; it opens a PR and stops, riding
  the same human merge gate every other worker does.
- **The toggle already exists.** `Guardrails.advanced_orchestrator` gates
  whether a repo's `.loomux/workflow.yml` blocks run at all; a role-hinted
  block exists only when that toggle is on for the launch *and* the repo
  declares it. The launcher roster preview is the consent moment — no new
  global switch.

## Personas + template fragments (slice C)

Two default personas, both `mode: replace` (no `model:` — the block pins it,
per the module docs in `profiles.rs`):

- **`.github/agents/advisor.md`** — read-only, consulted on demand. Reports
  advice via `report("done", ...)` and exits; never merges, spawns, or
  records a verdict.
- **`.github/agents/process.md`** — reviews one finished session cold via the
  `session_digest` MCP tool (referenced by name; stable regardless of slice
  B's internals), filters findings through "would a fresh worker on a
  different task hit the same wall?", and categorizes each durable learning
  into the four destinations from the table below. Opens a normal PR and
  stops — the same human merge gate as any other worker.

Neither file is loaded automatically by `role_hint` — a block still opts in
via `profile: .github/agents/advisor.md` in `workflow.yml`, exactly like any
other persona. `role_hint` drives three things *independent of which persona
file (if any) a block uses*:

1. **`mechanics_core(kind, role_hint)`** — the non-overridable core a `mode:
   replace` persona always gets now carries a role_hint-keyed addendum: an
   advisor-hinted planner still hears "you hold NO authority… you never
   merge, spawn, or record a verdict" even if its own replace persona forgot
   to say so; a process-hinted worker still hears "that PR rides the same
   human merge gate… you never merge it." Mirrors how the reviewer's verdict
   duty already rides in the core so a replace persona can't drop it.
2. **`ADVISOR_NOTE` / `PROCESS_NOTE`** (`templates/workflow.md`, rendered into
   the orchestrator's `{{WORKFLOW}}`) — line-final fragments, present only
   when the roster declares the matching `role_hint`, teaching the
   orchestrator how to spawn a consult (`spawn_agent(block: "<id>", task:
   "<question>")`) or a post-merge process-pro run.
3. **`ADVISOR_CONSULT_NOTE`** (`templates/worker.md`) — a line-final fragment
   telling every worker it can `message_orchestrator` to request a consult
   when the roster declares an advisor block; a worker cannot spawn the
   advisor itself.

All three are byte-empty when the hint they key on is absent — a golden test
(`advisor_and_process_prose_stays_silent_unless_a_block_declares_the_hint`)
pins that a fully custom roster with no `role_hint` block renders no mention
of "consult" or "process-pro" anywhere, and the existing
`tests/fixtures/pre222/*` byte-golden needed no re-bless: the rendered text
for a hint-free group is unchanged. The one thing that *did* need updating
was the placement-pin test itself (`worker.md` now chains two placeholders,
`{{BLOCK_NOTE}}{{ADVISOR_CONSULT_NOTE}}`, on the one line the old single-key
check didn't anticipate — the same chaining `block.md`'s `PERSONA_NOTE`/
`LANE_NOTE`/`GATE_NOTE` already relies on).

## Slices (see the plan comment on #250 for the full breakdown)

- **A — role_hint foundation**: the field, its parse-time validation, the
  capability-closure proof, and the launcher preview/roster chip. Landed
  first; everything else rebases onto it.
- **B — session digest** (parallel with A): the `session_digest` MCP tool and
  the friction-window extractor that normalizes Claude/Copilot transcripts.
- **C — personas/templates** (this note's newest scope, above): the default
  advisor/process personas, the workflow-conditional prose, and the
  `mechanics_core` addendum keyed off `role_hint`.
- **D — process-pro wiring**: the end-to-end demo of a merge triggering a
  process-pro spawn and a proposed skills/lessons PR, plus confirming the
  `gh` shim refuses a merge from that pane.
- **E — supervisor push-nudge** (optional): a deterministic, LLM-free
  audit-tail watcher that nudges the orchestrator toward a consult on a
  friction signature.

## How #324 relates to `.loomux/lessons.md` (#268)

#268 built only the passive substrate — a repo-committed, capped prose file,
injected at orchestrator kickoff, with no auto-extraction and no retrospective
agent ("a human or an agent edits the file like any other"). #324's
process-pro is the agent #268 anticipated: it extends the substrate, it does
not replace or duplicate it. It categorizes a learning and routes it to the
destination that already exists for that shape, always via a normal,
human-gated PR — there is no loomux "skills injection" runtime to feed; every
destination below is loaded natively by the tool that reads it:

| Learning shape | Destination | Loaded by |
|---|---|---|
| One-off repo quirk, prose | append `.loomux/lessons.md` | loomux, injected at orchestrator kickoff (#268) |
| Reusable, invokable procedure | new `.claude/skills/<name>/SKILL.md` | the Claude CLI, natively |
| Always-true rule / convention | patch `CLAUDE.md` / `AGENTS.md` | Claude / Copilot, natively |
| Persona / lane tweak | patch `.github/agents/<block>.md` | the block that references it |

`.loomux/lessons.md` is a small rolling buffer (capped, oldest-drop) with no
structure and nothing invokable — right for a one-line quirk, wrong for a
growing procedure or a rule that must never age out. This table (and the
"would a fresh worker on a different task hit the same wall?" durability
filter) already ships as `.github/agents/process.md`'s own prose (slice C);
what slice D adds is the end-to-end run — an actual merge triggering a
process-pro spawn, reading real GitHub ground truth, and opening a real
skills/lessons PR.
