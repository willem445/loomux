# Relevant-first board order, and where "cleared" lives (#1152)

A long-lived group's task board is mostly history. The board that prompted this
carried 400+ rows, nearly all `done`, with the handful anyone could act on
scattered among them in creation order — so the human scrolled past hundreds of
finished items to reach anything live.

Two mechanisms answer that, and the whole design rests on keeping them separate.

## 1. Sinking — derived, automatic, stored nowhere

Within **each sibling group** (the top-level rows, or one container's children),
the board renders the live rows first — in exactly the array order they already
had — then the finished ones, most recently updated first.

Three properties make this safe to turn on for everyone with no opt-in:

- **It is a stable partition, so priority order survives.** Board order *is*
  priority order and "top = next" is a contract the orchestrator reads. A stable
  partition never reorders within a class, so no two live rows ever swap.
- **It sinks *subtrees*, not statuses.** A row sinks only when it is `done`
  **and nothing unfinished is nested inside it**. Sinking a container takes its
  whole subtree with it, so keying on the row's own status alone would push live
  work off the bottom of the board — the one failure mode that would make this
  worse than the problem it solves.
- **Nothing is written.** It is a read-time projection like `isReady`, computed
  from data the board already has.

### The consequence for ▲/▼

Once finished rows sink, the stored array can hold any number of them *between*
two live rows. A move computed against the array would then step onto one of
those, and the click would change nothing on screen — a dead button.

So the step is taken against the **displayed** list of manually-ordered
siblings, and applied to the stored order as a **minimal splice**: the moved row
is lifted out and dropped immediately beside its displayed neighbour, and every
other row — settled ones included — keeps its relative place. The projection
never rewrites priority data as a side effect of a click, which is the line that
keeps it a projection at all.

On a finished row both arrows are **off**. Its position is derived
(most recently updated first), so a manual step there would either do nothing visible or
contradict the order the board just told the human it was using. Reopening the
row puts it back in the manual list.

## 2. Clearing — the human's explicit archive, and it IS stored

`cleared_ms: Option<u64>` on the task, stamped by the human's own commands.

### Why on the task, and not a UI-side set in the group dir

The alternative considered was a per-group file of cleared ids owned by the
frontend. It is not the smaller option once the plumbing is counted — the
frontend cannot touch disk, so it needs a new file, a read command and a write
command either way, i.e. the same command surface — and it is worse on the two
things that matter here:

- **It can drift.** A second store keyed by task id has no delete-strip: delete
  a task and its id lives on in the cleared set, to be silently reused by
  nothing or (worse) matched by a later row. `cleared_ms` on the row cannot
  drift from the row, because it *is* the row.
- **It splits the audit story.** The issue asks for traceability. A stamp on the
  task travels with the task through `orch_tasks`, the audit log's task
  snapshots, and every backup of `tasks.json`; a sidecar file is a second thing
  to keep, back up and reason about.

The usual objection is that this puts *view state* into board data, which the
board deliberately avoids: `expanded`, `collapsed`, `selected` and the
show-cleared toggle itself are all frontend-only, "a view preference that
survives re-renders but never becomes board data". The distinction is durability
and scope, not taste. Those four are per-window and per-session — which rows
*this* window is currently showing. "I have acknowledged this item and want it
out of my working set" is neither: it is a human-authored decision about the
work item, it must survive a restart, and it must be the same in every window
and pane that shows the board. That is board data by the same test `status` is.

### What it deliberately does not touch

- **`updated_ms`.** `filter_done_rows` picks the newest `LIST_TASKS_DONE_CAP`
  `done` rows *by* `updated_ms` (#865). Stamping a fresh `updated_ms` onto 250
  rows would silently rewrite which twenty the orchestrator sees on its next
  `list_tasks` — a human view action reaching into an agent's read. Clearing
  composes with that cap by leaving its input alone, and a test pins the kept
  set as identical across a clear.
- **Any agent-facing read.** The compact `list_tasks` row (`TaskSummary`) does
  not carry the field and does not filter on it, so the done-cap keeps meaning
  exactly what it meant; and the full-record read, `get_task`, goes through
  `AgentTaskView` (see below) rather than serializing the stored `Task`. Nothing
  agent-facing can start gating on the archive because nothing agent-facing can
  see it.
- **The orchestrator's attention.** `notify_board_edit` exists to say the queue
  moved, and this moves nothing: no status, no priority, no link, nothing
  `TaskSummary` even carries. This follows the `reorder_tasks` precedent (a
  board write the orchestrator is deliberately *not* interrupted for), not the
  `delete_done_tasks` one. It is recorded in the audit log, which is where a
  view action belongs.

### Human-only, by construction

`cleared_ms` is written by `orch_clear_done_tasks`,
`orch_restore_cleared_tasks` and the human board's `orch_upsert_task` — all
Tauri commands, none of them MCP tools. `mcp.rs` spells the new `TaskPatch`
field out as `None` rather than sweeping it up with `..Default::default()`, so
an agent cannot reach it and a future patch field cannot leak there by
omission. An agent tidying rows out of the human's sight is the one thing this
feature must never become.

### Human-only on the READ side too — and why that needed a type

The write side above was airtight from the start. The read side was not, and it
is worth recording exactly how it failed, because the shape recurs.

`get_task` returned `serde_json::to_string(&task)` — the stored `Task`, straight
onto the MCP surface. `cleared_ms` carries `skip_serializing_if =
"Option::is_none"`, which omits the key while the stamp is *absent* and emits it
the moment one *exists*. So every claim on this page, in `docs/orchestration.md`,
in `Task::cleared_ms`'s own doc block, and in the name of the test meant to pin
it was true right up until the human clicked 📥 — at which point any agent
calling `get_task` on that row got `"cleared_ms": <ts>` back. Four surfaces
agreed with each other and disagreed with the code.

The fix is not a filter, because a filter is only as good as the next person
remembering it. `AgentTaskView` is the agent-facing projection of a full task
record, and `agent_task_view` builds it by destructuring `Task`
**exhaustively**. Adding a field to `Task` therefore does not widen what agents
see — it stops the crate compiling until somebody classifies the new field:
name it in the view (agent-visible) or bind it to `_` beside `cleared_ms`
(human-only). Default-deny, enforced by the compiler rather than by care, and
the same posture `TaskSummary` already takes for the compact row.

Deliberately *not* also guarded by a source scan. The scan that would catch a
future `to_string(&task)` has to key off the binding's name, and this repo's own
convention rules that out — "a source-scanning guard must not decide from a
binding's name; a rename steps over it, so it enforces nothing". The exhaustive
destructure is stronger and rename-proof.

The lesson generalizes past this field: **a storage type with `#[derive(Serialize)]`
is not a wire shape.** Serializing one onto an agent-facing surface publishes
every field it will ever gain, and the publication is invisible at the call site
that causes it.

### Read-time, so nothing needs repairing

A row counts as archived only while the stamp is present **and the row is still
`done`**. Reopening a cleared task therefore brings it straight back into view
with nothing having to remember to wipe the stamp — the same
no-repair-pass discipline `ready` and the dependency chips follow. Clear it
again and the stamp is refreshed.

The archive's hide rule uses the same whole-subtree closure as the sink: a
cleared container still holding a live child stays on the board, because it is
the only thing on screen that says where that child lives.

## Row indent means exactly one thing

The `● ACTIVE — <agent>` badge used to be the first element of the row's flex
line, which pushed the task id and everything after it right by the badge's own
width. An active row therefore read as *indented* — indistinguishable at a
glance from a row nested inside a container, which is a real and separate signal
here (`.task-depth-N`, from the hierarchy above). The badge now sits after the
id. The glow, pulse and left accent carry "active"; the badge names *who*. A
row's left edge is decided by its depth and nothing else.
