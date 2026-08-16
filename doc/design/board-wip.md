# Design: board WIP limits (#1175)

Status: implemented (PR #1182). Config in `loomux-engine::workflow` (`BoardPolicy`,
`RawBoard`/`RawWip`), accounting and enforcement in `orchestration::mod`
(`wip_occupants`, `wip_entry_breach`, `OrchRegistry::upsert_task_from`), agent surface in
`orchestration::mcp` (`list_tasks`, `upsert_task`), chrome in `src/wipchips.ts` +
`src/tasksview.ts`.

## Problem

**Max live agents caps concurrent agents; nothing caps concurrent work.** An orchestrator
can hold four agents and still pile up ten items in `review` and `pr` while starting new
tasks — review debt, which the 2025 DORA report measures as the AI-era instability
signature (throughput up sharply, incidents per PR up far more). "Finish before you start"
was prose in the orchestrator template; kanban's answer is a per-column WIP limit, and it
is a mechanism.

The research that filed this is #1170 candidate A2, which also fixes the shape: repo
config in `.loomux/workflow.yml`, absent block = feature off, enforcement at the same
validation seam that refuses dependency cycles.

## Config

```yaml
board:
  wip:
    in-progress: 4
    review: 3
  enforce: false   # the default; omit it
```

An absent `board:` block means **no caps at all** and behaviour byte-for-byte unchanged —
the posture `resources:` and `merge_queue:` already take, including the documented
consequence that *adding* the key breaks the file for builds that predate it
(`RawWorkflow` is `deny_unknown_fields`, and a policy key an older build does not
understand means a human believes a policy is in force that is not).

### Why a closed struct, not `BTreeMap<String, u32>`

`RawWip` has one `Option<u32>` field per cappable status rather than an open map, and this
is the design's load-bearing small decision. An open key namespace **cannot tell a typo
from a status a newer loomux might have**: `in-porgress: 4` would parse, declare a limit on
nothing, and stay silent for the lifetime of the file. The closed struct hands that check
to `deny_unknown_fields`, whose error already names every field it would have accepted — so
the repo that misspells a status is told which spellings exist, at parse time, and this
module writes no check of its own.

The price is that the field list is a second copy of `TASK_STATUSES`, which lives in
`src-tauri` — on the other side of an arrow the engine crate may not point back along, so
it cannot be derived. It is pinned instead, in both directions:
`every_task_status_except_done_can_carry_a_cap` asserts the struct's serde field names are
exactly `TASK_STATUSES` minus `done`. A ninth status reddens rather than quietly arriving
uncappable.

Downstream, the parse produces a `BTreeMap<String, u32>` keyed by the wire status name. The
struct exists to *validate*; the map is the shape the rest of loomux reasons about, so the
accounting, the refusal text and the board chips are all status-generic.

### Why `done` cannot be capped

`done` is terminal and it is the **relief valve**: every other cap is relieved by work
reaching it. A limit there would refuse the very transition that unblocks the board — the
exact inversion of what a WIP limit is for — and under `enforce` it would wedge the board
rather than pace it. It has no field, so it is a parse error rather than a rule to
remember (`WIP_UNCAPPABLE_STATUS` is the one spelling the docs, the error path and the pin
all read).

`blocked` **is** cappable, deliberately: too much blocked work is a real signal and warn
mode is the right way to surface it. But an *enforced* cap on `blocked` refuses an agent's
report of reality, so the docs and the schema help text both recommend leaving it uncapped
under `enforce: true`. That is advice, not a rule — a repo that means it may write it.

### Why no grouped cap (`review+pr: 3`)

The issue floated grouping two statuses under one cap, which is where the practice
sometimes goes. Rejected, for three reasons:

1. **The board's display unit is the status.** #1105 gives the board kanban columns, and a
   column header showing `3/3` is the standard affordance. A cap spanning two columns has
   no honest column header: either two headers show the same number (which reads as two
   caps) or one shows it (which reads as a cap on one column).
2. **Two numbers say the same thing.** "At most 3 items downstream of coding" is
   `review: 2, pr: 1`. The grouped form buys the ability to trade one for the other; it
   costs the ability to say which stage is the bottleneck, which is the thing the human is
   actually looking at the board to learn.
3. **The config would have to grow a level** — `wip: { name: { statuses: [...], limit: N } }`
   — which loses the exact shape the issue itself wrote (`wip: { in-progress: 4 }`) and
   loses `deny_unknown_fields` as the status-name check along with it.

It is not foreclosed: an additive sibling key could express groups later without changing
what is here.

## Enforcement: one seam, judged on entry, counted on leaves

The check sits in `upsert_task_from`, last in the run of pre-mutation validations that
already refuses dependency cycles, hierarchy cycles and failed claim guards — after the
claim guards deliberately, so a claim refused because the task is already held says *that*,
not a board-pacing limit the caller was never going to reach. Nothing is persisted until
`write_tasks` at the bottom, so a refusal leaves the board exactly as it was, which is the
same contract every other refusal there keeps.

**Only an entry is judged.** A patch that leaves a row where it already is — a note, a PR
ref, a re-assertion of the current status — crosses nothing and is never refused. Neither
is any move *out*. This is what keeps an over-limit board (a cap lowered under live work, a
human edit, warn mode doing its job) workable rather than frozen: every write that relieves
a cap still lands, and only adding to one is judged. A `claim` counts as an entry into
`in-progress`, which is the motivating case — an orchestrator starting new work while
review debt grows *is* a sequence of claims — and it reaches the status by its own route,
so a check that read `patch.status` alone would have left exactly that case unguarded.

**Only leaf rows are counted.** A container's status is a rollup of the work its children
carry, so counting a `feature` in `in-progress` *and* the three stories under it counts the
same work twice, and makes `in-progress: 4` mean four items on a flat board and rather
fewer on a nested one — a cap nobody can reason about. #1156 is making nesting the normal
shape, which makes this the difference between a cap that means something and one that
drifts with how the board happens to be structured. The row being written is exempt from
the check entirely when it is itself a container, for the same reason: what it carries is
counted where the work is.

`wip_occupants` is the **one** definition of what a cap counts, shared by the seam that
enforces it and by every surface that displays `n/N`. A second tally in the frontend would
be a second definition, and the first time either side learned something about containers
the board would be showing a number its own refusals disagree with.

## Warn by default; enforce is opt-in

`enforce: false` (the default, and what an omitted key means) makes a crossing **land**,
audit under `task-wip-crossed`, and deliver a notice to the orchestrator's pane. That
notice is not decoration — under the default posture it is the *entire* effect of the
feature, so an orchestrator that never reads it respects no cap at all. That is why
`orchestrator.md` gained a bullet in the same change: a warning changes nothing unless the
agent whose queue discipline it is has been told to read it.

Warn-first is the right default because a limit is a *guess* until a team has run under it.
A repo that discovers `review: 3` is wrong learns it from three notices, not from a week of
refusals. The refusal is what you turn on once you believe the number.

## Human writes are never refused

Under `enforce: true` an agent's entry past a cap is refused. **The human's own board edit
is not, under either setting.** The rule is not a carve-out bolted on; it is the same rule
the board already keeps: *the board's authority is the human's, not a queue discipline* —
which is why `claim` is deliberately not exposed on the human's board command at all. A
limit a human declared for their agents must not bounce the human who declared it, and a
human moving a card is very often the act of *resolving* the overload the cap flagged.

Their crossing still warns and still audits (with `origin: "human"`), so the orchestrator
learns the board moved past a cap whoever moved it. It is only ever the refusal that is
agent-only.

Origin reaches the seam as a **parameter**, not as a look at `actor`. Every human path
passes the literal string `"human"` today, so an `actor == "human"` test would work — and
would be a guard a rename steps straight over, which is the failure mode this repo's
source-scanning-guard convention exists to prevent. `OrchRegistry::upsert_task` resolves to
`WriteOrigin::Agent` and `upsert_task_by_human` to `Human`, so the *stricter* posture is
the unnamed default: a new call site that forgets to think about origin gets the one that
can only ever refuse too much, and a refusal is visible.

## Reading the policy

`board_policy` mirrors `merge_queue_policy` exactly: `self.group()`, the
`advanced_orchestrator` guardrail, then a live `load_workflow` — no cache, so an edit to
the file takes effect without a restart. Two consequences worth stating:

- **It is read before `tasks_lock` is taken**, the way `with_locks` reads `lock_resources`
  before taking the lock table. `tasks_lock` is a process-global mutex serialising every
  group's board write, and a YAML open-and-parse does not belong inside it. The read is
  skipped outright for a write that cannot be an entry (an existing row, no status, no
  claim), so the hot path — a note append, a `pr` ref — stays exactly as expensive as it
  was before this feature existed.
- **An unreadable or unparseable file resolves to no caps.** That is fail-open, which is
  the wrong direction for a security check and the right one here: a WIP limit paces work
  and guards nothing (the human merge gate is not reachable from this block at all), and a
  file caught mid-save already makes the whole workflow — gates included — unenforceable
  down the loud `workflow-invalid` path. Wedging every board write behind half-written YAML
  would be a far larger claim than a pacing discipline earns.

## Surfaces

- **`list_tasks`** carries `wip: [{status, limit, count, enforce}]` beside the rows —
  empty for the repos, most of them, that declare none. It rides the board read rather than
  getting a tool of its own because a cap is only actionable next to the rows it is about;
  an orchestrator that had to make a second call would learn the limit after deciding.
- **`orch_workflow_status`** carries the same rows, computed from the same function, for
  the board pane. `refresh()` reads it in the same pass as the rows, so a chip is at most
  one refresh out of step with the list under it — the same best-effort enrichment the
  live-agent set and the question markers already are. It is a count beside a live board;
  it authorises nothing.
- **The board header** renders `review 3/3` chips (`src/wipchips.ts`, DOM-free and unit
  tested). On the header and not on a status column because this board *has* no status
  columns: it is one priority-ordered list whose order is meaning. Inventing columns for a
  feature most repos never turn on would reorder the board around it. #1105 is where the
  columns arrive, and `WipChip` already carries exactly what a column header needs.

Three fills, not two: **full** (`count >= limit`) is the state the practice is about —
start nothing new — and **over** additionally means the board is somewhere a cap says it
should not be. A two-state chip would say nothing until the board was already past the
limit.

## What this deliberately does not do

- **It does not gate merges.** Nothing in `board:` is reachable from the merge gate, and
  `deny_unknown_fields` makes that structural rather than a promise.
- **It does not move work.** No auto-flip, no queueing, no "hold this until review clears".
  The board says what is true; the discipline is the agent's and the human's.
- **It does not cap agents.** That is *max live agents*, which is a different question with
  a different answer, and the point of this feature is that the two are not the same.
