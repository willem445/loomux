# Design: task-board Agile hierarchy (#958)

Status: **implemented.** The backend landed first (#994) — `Task::parent`/`Task::kind`,
write-time validation, promote-on-delete, and the `TaskSummary`/MCP surface described below —
and the board nesting UI (#1027) landed after its demo. Symbols are the durable reference.

## 1. Problem & thesis

Before this feature, one task on the board meant one feature-area: a project with several
ordered slices (an issue like `#879`, holding slices A–M) collapsed into a single coarse row,
with the real ordering — B before K before L, B before M, A before C — left in prose notes and
`blocked` statuses. `deps` (#582) links *tasks*, and a slice wasn't a task, so `ready` could
never reflect true slice-level startability, and a human reading the board saw one blob instead
of a feature's actual shape.

**Thesis:** give the orchestrator a way to create a container task (an Epic or Feature) and
hang concrete slices beneath it as ordinary child tasks, each with its own `deps`. That turns
"what's startable right now" into a real per-slice signal instead of something re-derived from
notes after every restart, and it gives a human glancing at the board the feature's shape —
children, progress, blockers — at once. It is also the data foundation a future kanban/
swimlane view would need, though that view is explicitly out of scope here (§10).

## 2. Model: containment is orthogonal to ordering

`Task` gains two additive fields, both `#[serde(default, skip_serializing_if =
"Option::is_none")]` so a pre-#958 `tasks.json` loads unchanged and a board that never nests
anything never gains either key — the same zero-migration shape #582 shipped for `deps`/
`related`:

- **`parent: Option<String>`** — the id of the task this row sits *inside*. This is
  **containment**, not ordering: a Feature "contains" its slices the way a folder contains
  files, independent of which slice must finish before which other one starts. `deps` stays
  exactly what it was — ordering, checked at read time for readiness — and the two are
  deliberately orthogonal: a dep may cross subtrees or link two containers, and none of the
  #582 link machinery (`normalize_links`, `find_dep_cycle`, `strip_deleted_links`) consults
  `parent` at all.
- **`kind: Option<String>`** — an advisory Agile level from `TASK_KINDS = ["epic", "feature",
  "story", "task"]`, validated exactly like `status` against a closed vocabulary. Absent means
  "a plain task", which is what every row written before this field existed already is.

Both fields are stored **on the pointing side only** — there is no `children` array on the
container. That mirrors how a `deps` edge is stored: one source of truth, one delete-strip
bookkeeping, rather than two structures that could drift apart. A `children` array was
considered and rejected for exactly that reason.

### Levels are advisory, not enforced

Nothing stops a `story` sitting directly under an `epic`, skipping `feature`. Enforcing the
ladder would buy no data integrity — every reader needs the actual tree, not level discipline —
would fight the dominant real shape (a Feature plus its slices, no intermediate Story rows), and
would complicate reparenting and kind-less rows carried over from before this feature existed.
It stays one write-time check to add later if the discipline is ever wanted, which is the cheap
direction to leave open; retracting an enforced ladder once agents and humans depend on it would
not be cheap.

## 3. Write-time validation

`upsert_task` validates a `parent` write the same way it already validates a `deps`/`related`
write — entirely **before the mutable borrow**, so any rejection leaves the board exactly as it
was:

- the named container must be a live task on this board, and never the row itself;
- **cycle rejection**: reparenting a row under its own descendant is the cycle case, checked by
  an ancestor walk with the new edge substituted in. The error names the path, in the same style
  `find_dep_cycle` already uses: `parent: hierarchy cycle t-1 → t-3 → t-2 → t-1`;
- **depth cap**, `MAX_TASK_DEPTH = 4` (the epic → feature → story → task ceiling) — checked
  against the *deepest resulting row*, not just the mover: the new ancestor-chain length plus
  the height of the moving row's own subtree. Checking only the mover's new depth would let a
  two-level subtree land at depth 4 and silently put its own children at depth 5;
- a container that also appears in the same write's `deps`/`related` is refused — a link to your
  own container is always a mistake. This check is scoped to the fields the write actually sets
  (a `parent` write is judged against both link arrays, since that write is what creates the
  overlap; a link-only write is judged only against the array being written), because the wider
  reading refused writes that had not created the overlap — a row may legitimately dep on its
  grandparent, and promote-on-delete (§4) can turn that grandparent into its direct parent
  later, which is a legitimate state, not a mistake to re-refuse on the next unrelated edit.

Both ancestor-walk and subtree-height walk are cycle-tolerant by construction (a repeat check on
the former, a visited set on the latter's breadth-first walk), because a hand-edited
`tasks.json` is the one board that can already be cyclic, and the walk has to terminate on it
regardless of how it got there.

## 4. Promote-on-delete

Across all three delete paths (`delete_task`, `delete_tasks`, `delete_done_tasks`), inside the
**same locked write** as `strip_deleted_links`: a survivor whose container was just removed is
reparented to the **nearest surviving ancestor** of that container, or promoted to top level if
the whole chain up to the root was deleted in the same write. The audit row carries
`reparented: [ids]` beside the existing `relinked`.

**Promote, not cascade, not refuse** — the same reasoning `strip_deleted_links` already applies
to dependency links: refusing the delete fights the human's authority over a board they can
hand-edit; cascading would silently destroy work items along with their PR and session
references, the worst failure direction available. Promotion only loses the grouping.

The walk climbs the *removed* chain rather than reading one stored pointer, because a batch
delete can remove a parent and its own grandparent in the same write; reading a single pointer
would land the child on a row that same write just deleted. That case is pinned directly by a
test that deletes a parent and grandparent together and asserts the surviving grandchild lands
on the next-surviving ancestor, not on either deleted row.

## 5. Read tolerance — no repair pass

Unlike an unknown `deps` id, which reads as *unmet* because deps gate readiness (§6), an unknown
or cyclic `parent` is **display-only**, so the safe failure direction is to tolerate and show
rather than hide or refuse to render. A hand-edited orphan, self-reference, or cycle in
`tasks.json` blocks nothing backend-side, and the invariant the board UI (#1027) holds for all
three is that **every row appears exactly once — never dropped, never looped** —
the same tolerate-and-show philosophy the existing `⚠ missing` dependency chip already applies to
a dangling `deps` id. It is *not* the same rendering for all three: an orphan or a self-reference
renders at the top level, while a cycle's members render once each, with only the first one
reached at the top level and the rest nested underneath it (§9 spells out both the render
difference and the narrower chip). The chip itself is narrower still — it fires only for the
orphan case.

No migration and no repair pass exist or are planned: this is a deliberate consequence of the
additive-serde, tolerant-read design, not a gap.

## 6. Rollups and readiness — derived, never stored

`TaskSummary`/`board_summaries` — the projection `list_tasks` returns (#245's compact-row
discipline) — gain, read at call time and never persisted:

- `parent`, `kind`, mirrored straight from the row, skipped when absent;
- `children` / `children_done` — **direct children only**, counted in the same per-call board
  scan `board_summaries` already does for `ready`. This is deliberately *not* a subtree rollup:
  a subtree count would have to answer what a hand-edited parent cycle rolls up to, where a
  count of "rows that point here" has no such question. It is also deliberately *not* a nested
  child list — that is exactly the payload-size expansion #245 exists to prevent, and the tree
  itself is one client-side pass over `parent` on a board `list_tasks` already returns whole. A
  dedicated get-children/tree MCP tool was considered and rejected for the same reason.

**`ready` is untouched in this shipped slice.** `task_ready`/`unmet_deps` remain byte-identical
to their pre-#958 form: a child of a `blocked` container, or a container with unmet deps of its
own, is still `ready` exactly as before. Folding ancestor state into readiness ("a child is not
ready while its container is blocked") is a semantics decision that needs the human's sign-off —
it changes what every existing board's `ready` badge means — so it is left **visible but
unenforced** rather than arriving as a side effect of the data model. A test
(`hierarchy_does_not_change_the_readiness_truth_table`) pins this as a deliberate deferral, not
an oversight.

**Auto-status rollup is rejected, not deferred.** A Feature automatically flipping to `done`
once every child is `done` was considered and rejected outright: `status` already has two
authors (a human and the orchestrator, each behind its own claim/approve guard and audit trail),
and a derived write-back would recreate the exact wedge class the #582 design avoided by making
`ready` a pure read-time projection instead of a stored status. Instead, the board UI badges a
container whose children are all done but whose own status lags, as a **nudge** — never a status
mutation.

## 7. Metadata-only stance — nothing gates on `parent`/`kind`

**`parent` and `kind` are display and queue-hint metadata only. Nothing that decides whether an
action may happen is ever allowed to read them.** This is the same argument CLAUDE.md constraint
6 and the existing `Task::pr_base` field already establish: the task board is agent-writable, so
a gate that trusted board data would be a gate the thing being checked gets to answer. Nothing
in the merge gate, the `gh` shim, or any future merge queue reads hierarchy fields, and nothing
should ever be added that does. What hierarchy legitimately buys is a more accurate story for a
human glancing at the board, and a queueing hint for the orchestrator — neither is an
authorization.

## 8. Surface (MCP + board)

- `upsert_task` gains `parent` (task id string; empty string clears — promotes to top level,
  the same clear-on-empty rule `pr` already uses) and `kind` (enum `epic | feature | story |
  task | ""`, `""` being the clear). Both take the strict arg parser, so a wrong-typed value is
  refused rather than silently dropped or coerced. The tool description teaches the intended
  pattern: create the epic/feature once, hang slice tasks under it with per-slice `deps`, then
  read `ready` at slice granularity instead of re-deriving the queue from prose.
- `remove_task`'s description documents promote-on-delete, so an agent deleting a container
  knows its children survive and where they land.
- `list_tasks` rows pick up `parent`/`kind`/`children`/`children_done` through the same
  `board_summaries` projection every row already goes through; `get_task` gets the new fields
  for free since it returns the full record.
- The human-facing `orch_upsert_task` command gains `parent`/`kind` as additive optional
  params — omitted deserializes to "leave alone", so every existing board-edit call site keeps
  working untouched. `orch_reorder_tasks` needed no change: it already takes the whole board's
  id array, and a subtree-preserving reorder (§9) is a client-side concern over that same array.
- No new MCP tool was added — a get-children/tree tool was considered in the investigation and
  rejected both times (§6): `list_tasks` already returns the whole board, and the tree is one
  client-side pass over `parent`.

## 9. Board UI (#1027)

This slice was visible UI, so it was demo-gated: held for a human's own eyes on the running app
before merging, because the nesting affordances are the kind of thing that reads fine in a diff
and wrong on screen. The demo passed with one change — the collapse chevron was sized up and
given a resting background, having been legible-but-easy-to-miss at its first size. What follows
is what shipped.

- **Derived display order.** `tasks.json` stays a flat array, and its order stays the priority
  order used everywhere else on the board; the tree is derived at render time from `parent` —
  roots in board order, each followed by its own subtree, recursively. A child stored above its
  container in the raw array still renders nested under it.
- **Collapse.** A chevron appears on containers only (a leaf gets an inert spacer, so the
  affordance itself communicates "there is something inside here"). Collapsing hides the whole
  subtree, not just the direct children — leaving a grandchild rendered at the top level would
  read as data loss. Collapse state is frontend-only: not persisted, the same shape as the
  existing `expanded` (note-expansion) state, and pruned to currently-live rows on each
  refresh, the same way the existing `selected` (tick-box) state already is — two different
  existing behaviours, not one.
- **Kind badge.** The advisory Agile level, shown as a label and nothing more — no enforcement
  rides on it (§2). A value outside the four known kinds (only reachable by hand-editing
  `tasks.json`, since the backend refuses it on write) reads as visibly broken rather than as a
  silent fifth level.
- **Rollup chip + nudge.** A `done/total` chip for **direct** children — the same two numbers
  the orchestrator's own `list_tasks` row carries, so a human's board and the orchestrator's
  view of the same container never disagree. A separate "all inside done" nudge requires the
  **whole subtree** done, not just the direct children, because it makes an unqualified claim
  ("everything under here is finished") that direct-children-only could get wrong with an open
  grandchild. It is a prompt for the human to act on, never a status write (§6).
- **Nest / un-nest.** A picker offers every other row **minus the row's current container** as
  a possible new one — which is why a separate "promote to top level" option exists, sending the
  `parent`-clearing empty string when the row is already nested. This is deliberately a separate
  affordance from the existing dependency picker — containment is not ordering (§2), and
  conflating the two pickers would blur that. Deliberately absent: any cycle/depth pre-filter on
  the candidates offered. A row's own descendants are offered like any other row, exactly as the
  dependency picker offers a cycle-closing dep — the rule lives once, inside the backend's lock
  (§3), and its error names the path through this same picker's toast. A second, client-side copy
  of that rule could only ever disagree with the one that actually decides.
- **Sibling-scoped reorder.** Up/down move a row among its siblings only, carrying a
  container's whole subtree with it; the write is a full flattened permutation of the board's id
  array through the existing `orch_reorder_tasks`, since that command already expects the whole
  array and has no notion of siblings itself.
- **Read tolerance in the UI.** As described in §5, every row appears **exactly once — never
  dropped, never looped** — regardless of what a hand-edited `parent` says. The
  *rendering* differs by case, though: an orphan (names no task on the board) or a self-reference
  (names the row itself) renders at the top level, at depth 0. A cycle does not: `buildTree` lists
  a cyclic row as both a root and a child, so the first member `visibleRows` reaches renders at
  the top level and every other member of that cycle renders nested underneath it — a 3-cycle is
  three levels deep, not flat. The `⚠ in t-N` broken-container chip is narrower again: it fires
  only for the orphan case, never for a self-reference or a cycle, both of which name only live
  rows, so nothing about them is "missing" in the sense the chip reports.
- **No nesting chrome on a board that nests nothing.** The collapse gutter and related chrome
  are gated the same way the existing dependency/readiness chrome is gated on `deps` usage, so a
  board that has never used hierarchy renders exactly as it did before this feature existed.

## 10. Explicitly out of scope

Filed as follow-ups rather than built here, each for a stated reason:

- **Kanban/swimlane board views** — columns by status, swimlanes by top-level container, a
  drag-between-columns status write. Needs nothing new from the data model shipped here; it is
  a view-mode addition on top of it.
- **Folding ancestor state into `ready`** — §6's deferred semantics decision, needing the
  human's sign-off before it changes what every existing board's readiness signal means.
- **A dedicated kind picker on the board UI** — #1027's brief was nest/un-nest and display
  `kind`; setting `kind` itself stays an MCP-surface (orchestrator) action for now.
- **Role-template edits** — none were needed for this feature; the MCP tool descriptions are the
  teaching surface for the orchestrator, so no `pre222` fixture re-bless was owed by this work.
