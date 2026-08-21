# Design: task-board Agile hierarchy (#958, #1156)

Status: **implemented.** The backend landed first (#994) — `Task::parent`/`Task::kind`,
write-time validation, promote-on-delete, and the `TaskSummary`/MCP surface described below —
the board nesting UI (#1027) landed after its demo, and slice R then folded ancestors into
`ready` (§6), the one semantics decision the first slice deliberately deferred. #1156 then
**overturned §2's advisory-levels position** and added kind-prefixed ids; §2 carries both sides
of that argument, since the reasoning that was retracted is the part a future reader most needs.
Symbols are the durable reference.

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
  `parent` at all. Orthogonal is not independent — readiness reads *both* (§6), because a slice
  inside a waiting feature is waiting too. What never happens is one becoming the other:
  containment is never stored, written or validated as an edge.
- **`kind: Option<String>`** — an Agile level from `TASK_KINDS = ["epic", "feature",
  "story", "task"]`, validated exactly like `status` against a closed vocabulary, and since
  #1156 against the ladder in §2 as well. Absent means "a plain task", which is what every row
  written before this field existed already is — and which stays exempt from the ladder (§2.1).

Both fields are stored **on the pointing side only** — there is no `children` array on the
container. That mirrors how a `deps` edge is stored: one source of truth, one delete-strip
bookkeeping, rather than two structures that could drift apart. A `children` array was
considered and rejected for exactly that reason.

### Levels are enforced (#1156) — overturning this note's own earlier position

`ladder_rule` is the whole rule, as data: an **epic is top-level only**, a **feature must sit
directly inside an epic**, a **story inside a feature**, a **task inside a story**. The task
level is optional in the sense that matters — a story with no tasks under it is complete work,
not a gap — but a row *labelled* `task` owes a story, because that label is what claims it is a
sub-item of one. A write that breaks the ladder is refused, never coerced (§3).

**What this replaced, and why it was wrong.** #958 shipped the levels advisory and argued for
it here: enforcement "would buy no data integrity — every reader needs the actual tree, not
level discipline — would fight the dominant real shape (a Feature plus its slices, no
intermediate Story rows), and would complicate reparenting and kind-less rows". The cheap
direction, it said, was to leave the check to add later.

Two of those three held up. Enforcement genuinely buys no *data* integrity: every reader still
walks `parent`, and nothing downstream became simpler. Reparenting and kind-less rows genuinely
did need care — §2.1 and §3 are that care. What the argument got wrong is the premise underneath
all three: that the value of a level is what a *reader* can compute from it. The human's verdict
after using the shipped board is that the value is what a level makes a *writer* commit to. An
advisory `feature` label is a word an orchestrator picks per row, so the board accumulated a flat
list of confidently-labelled rows whose labels described no relationship to each other, and a
human glancing at it could not tell a real decomposition from a pile. Enforcing the ladder does
not make the tree more accurate; it makes the tree *mean something*, because the only way to
label a row `story` is to have said what feature it breaks down, and the only way to have a
feature is to have named the epic it serves. That is a claim about legibility, and legibility was
the whole thesis (§1) — so the advisory position was arguing against its own premise.

The one prediction that came true is the one that made this expensive rather than free:
"retracting an enforced ladder once agents and humans depend on it would not be cheap" — read in
the other direction, *adopting* it once boards exist is not cheap either, which is what §2.1 and
§2.2 and the whole of §3's trigger rule are paying for.

### 2.1 A level-less row is exempt — permanently, not transitionally

`ladder_rule(None)` is `Exempt`: a row carrying no `kind` may sit anywhere, may contain
anything, and may be promoted to top level at any time. Nothing about it is checked.

This is deliberately **not** a migration allowance that ages out once boards catch up, and it is
the load-bearing decision of the whole change:

- **A flat board must keep working forever.** loomux is a generic agentic-dev tool, and
  CLAUDE.md constraint 8 says its product code must not bake in one way of working. A group
  that runs no Agile at all — a queue of ten independent chores — has nothing to say with
  `epic`/`feature`, and a board that demanded the ladder from them would be a methodology
  shipped as a requirement. The level-less row *is* the no-methodology mode.
- **It is what makes the migration boundary land in the right place.** Every pre-#1156 board is
  level-less or partly so; exemption means the enforcement's blast radius is exactly the rows
  that made a claim about their level, and zero rows that did not.
- **It is the escape hatch from any refusal.** Every ladder error names two ways out, and one of
  them is always "clear its level" — which is reachable in one write from any state, needs no
  other row to exist first, and destroys nothing but a label. Without a permanent exemption there
  would be board states with no legal move out of them.

The one thing exemption does *not* buy is a levelled row inside a level-less one: "inside a
feature" is a claim about the **container**, and a container carrying no level does not make it.
That asymmetry is checked from both sides and is what stops the exemption becoming a hole the
whole ladder can be walked through — nest a level-less row anywhere you like, but you cannot use
it as a rung.

### 2.2 Ids: new rows are minted at their level, existing ids are never rewritten

A new row's id carries the level it was created at — `e-3`, `f-4`, `us-5`, `t-6`
(`kind_id_prefix`); a row created with no level mints `t-`, which is what every id on every
pre-#1156 board already is.

**One shared counter, not one per prefix** (`next_task_id`, a high-water mark over all four).
Per-prefix counters read nicer — the first epic would be `e-1` rather than `e-41` on a board with
40 legacy rows — but they let `e-1`, `f-1`, `us-1` and `t-1` all exist at once, so a half-
remembered "1" with a guessed prefix resolves to a **real but wrong row**: a silent mis-link in
`deps`/`related`/`parent`, which is precisely the confusion this feature exists to remove. With
one counter a wrong prefix names nothing and comes back `unknown task`. The cost is cosmetic and
the benefit is a wrong reference failing loudly.

**An existing row's id is never rewritten, whatever happens to its level.** This is the answer to
#1156's hardest question — whether to migrate existing boards onto prefixed ids — and the answer
is no, in both the one-time-migration sense and the re-level sense:

- An id is quoted from more places than the board can reach. Other rows' `deps`/`related`/
  `parent`, every `audit.jsonl` line that ever mentioned it, agents' stored session state and
  compacted context, PR bodies, and a human's own memory. A rewrite would have to be atomic
  across all of them, and the last three are not writable at all — so a "complete" migration
  would in fact be a migration that silently broke the references it could not see.
- The failure mode is the worst available. A dangling reference on this board reads as a missing
  dep or a broken container chip; a *rewritten* id that some old audit line still cites reads as
  a row that was never there, and there is no repair pass that could tell the two apart later.
- The cost of not migrating is one wart, stated plainly: **on a row whose level changed after
  creation, the id prefix and the level disagree.** A `t-7` re-levelled to `feature` stays `t-7`.
  The `kind` field is the authority and the board renders it as its own badge beside the id, so
  the disagreement is visible rather than misleading; the prefix means "where this row started",
  not "what this row is". Every doc surface that names the prefixes says so.

Nothing rewrites, repairs, or backfills a board on load — the same no-migration-pass stance §5
already takes for `parent`.

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

### 3.1 The ladder check, and what triggers it (#1156)

`check_ladder` judges one containment edge and produces the refusal; `upsert_task` calls it in
two directions, in the same read-only-before-the-mutable-borrow position as everything above.

- **The row's own link**, whenever the write touches `kind` *or* `parent`, judged against the
  **resulting** row (the patch's level if it sets one, the existing one otherwise; likewise the
  container) — never against the pre-state, so setting both in one call is judged as the pair it
  is. That is also how a create names its level and its container at once.
- **Its children**, whenever the write touches `kind`. A child's rule reads its container's
  *level*, not where that container itself sits, so only a re-level can invalidate one and a pure
  reparent skips the walk. This direction is the one a caller cannot see from the write it made,
  so the refusal names the child.

**The trigger is the write that asserts the shape, never the row's existence** — and that single
choice is the entire compatibility story. Judging every write would have frozen the `status`,
`notes`, `assignee`, `deps` and `claim` of every row on every board that used advisory levels,
which is exactly the shape §2 called dominant. Instead a legacy row stays fully editable, and only
a write that re-states its level or its container has to resolve it. Re-writing the level a row
already carries counts as such a write, deliberately: it is a fresh claim about where the row
sits, so the board answers it honestly rather than treating "no change" as "no claim". This is the
same narrowing the container/deps overlap check above already uses — a residual shape is tolerated,
and only a write that *re-asserts* it is refused.

**Every refusal names the fix**, the way the cycle refusal names the path. A ladder violation has
exactly two ways out — nest the row where its level belongs, or stop claiming that level — and an
error that only says no leaves the caller guessing between them. So the text carries the level
that is missing, the level the container actually is, and both moves.

**Promote-on-delete is the one path that can create a shape no write could ask for** (§4): a
`feature` whose epic is deleted lands at top level. Refusing the human's delete, cascading into
the work items, or silently stripping the survivor's level were all considered and rejected — the
last is data destruction to preserve an invariant about a label. The promoted row reads and
renders fine, is editable in every other field, and is resolved by the next write that touches its
own level or container. That is the strict-write/tolerant-read split (§5) applied to the ladder.

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
or cyclic `parent` **names no ordering constraint of its own** — since slice R only ever reads
the *deps* of the containers it finds, a chain that ends nowhere contributes nothing to check —
so the safe failure direction is to tolerate and show rather than hide or refuse to render. A hand-edited orphan, self-reference, or cycle in
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

**`ready` climbs the container chain (slice R).** The first shipped slice deliberately left
`task_ready` byte-identical to its pre-#958 form and filed the semantics decision as a
follow-up, because folding ancestors in changes what every existing board's `ready` badge
means. Slice R makes that change: `ready` is now `queued` ∧ every own dep `done` ∧ **every
ancestor's deps `done`**, derived by `blocking_ancestor`, which returns the *nearest* container
above the row that is still waiting (an id, not a boolean, so a caller can name what is holding
the row). The argument is the failure direction: a slice inside a feature that could not itself
start used to read `ready: true`, so the one call the orchestrator makes to answer "what can
begin now" answered with work that could not begin.

Three things that rule deliberately does **not** do, each of which is the more obvious reading:

- **An ancestor's `status` is never read — only its `deps`.** A container sitting at
  `in-progress` is the *normal* state while the work inside it runs, and `blocked` is by this
  repo's own convention the status for blockers *outside* the board (see `orchestrator.md`),
  which says nothing about whether the subtree can proceed. Reading either would make a slice's
  readiness a function of how promptly someone maintains the container row, where `deps` is a
  machine-checked ordering primitive. So a child of a `blocked` container *is* still startable,
  which is the one case where the shipped rule diverges from the motivating one-liner ("a child
  of a blocked parent"); the alternative is one clause away if the human wants it.
- **It does not touch the `claim` guard**, which still judges a row's own deps alone. Readiness
  is a hint; `claim` is a gate, and §7 binds gates. The consequence is visible and intended: a
  row can read `ready: false` and still be claimable, and a hand-edited container can therefore
  dim a row on the board without ever refusing a write.
- **It stays a read-time projection.** Nothing writes a status, so no hierarchy edit can wedge a
  task — the same property #582 bought by making `ready` derived in the first place.

The ancestor walk is `find_parent_cycle`, the one the write path already uses, rather than a
second walk: containment is a functional graph, one walk with a repeat check terminates on the
one board that can be cyclic, and two walks could only ever disagree about where a hand-edited
chain ends. Tolerance follows §5 exactly — an orphan container ends the chain and blocks
nothing (a row with no container has nothing to wait on), a cycle is walked once per member,
and a row is never its own blocking ancestor.

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

Slice R's readiness rule (§6) is on the **hint** side of that line and is the boundary case
worth stating outright, since it is the first thing to read `parent` at all: `ready` decides
nothing — it is a projection a reader acts on, and a reader who ignores it is refused nothing.
The gate next to it stayed put deliberately: `upsert_task`'s `claim` guard still judges the
row's own deps, so a hand-edited container dims a row and can never refuse a write. "Does a
wrong value here mislead a human, or open something?" remains the test, and hierarchy must keep
answering *mislead*.

**#1156's enforcement does not cross that line, and the distinction is worth being exact about**,
because "the write is refused" sounds like a gate. A *validator* judges the write itself: it
decides what shapes the board will store, exactly as the closed `status` vocabulary, the
live-task check on a `deps` id, and the cycle and depth-cap rules already do. An *authorization*
decides whether some action elsewhere may happen on the strength of what the board says. The
board is agent-writable, so an authorization that read it would be a check the thing being
checked gets to answer — which is why the rule is about authorizations and not about strictness.
After #1156 the set of things that read `kind`/`parent` to decide whether an action may happen is
still empty: not `claim` (still the row's own deps), not the merge gate, not the `gh` shim, not
the merge queue. What an agent gained is the ability to be told its own board write was
malformed; what it did not gain is the ability to open anything by labelling a row.

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
  container in the raw array still renders nested under it. (#1152 added a second derivation on
  top of this one: within each sibling group, finished subtrees sink below the live rows, and
  cleared rows drop out. Both are projections of the same untouched array, and the live rows'
  relative order is exactly what it was — see `doc/design/board-order-and-archive.md`.)
- **Collapse.** A chevron appears on containers only (a leaf gets an inert spacer, so the
  affordance itself communicates "there is something inside here"). Collapsing hides the whole
  subtree, not just the direct children — leaving a grandchild rendered at the top level would
  read as data loss. Collapse state is frontend-only: not persisted, the same shape as the
  existing `expanded` (note-expansion) state, and pruned to currently-live rows on each
  refresh, the same way the existing `selected` (tick-box) state already is — two different
  existing behaviours, not one.
- **Kind badge.** The Agile level, shown beside the row's id — and since #1156 the *authority* on
  what the row is, where the id prefix is only where it started (§2.2). Its tooltip is derived
  from `ladderRule` rather than written out beside it, so the sentence cannot go stale the way
  the word "advisory" did. A value outside the four known kinds (only reachable by hand-editing
  `tasks.json`, since the backend refuses it on write) reads as visibly broken rather than as a
  silent fifth level, and is treated as exempt by the ladder for the same reason the backend
  exempts it: no rule can say where a fifth level belongs.
- **Rollup chip + nudge.** A `done/total` chip for **direct** children — the same two numbers
  the orchestrator's own `list_tasks` row carries, so a human's board and the orchestrator's
  view of the same container never disagree. A separate "all inside done" nudge requires the
  **whole subtree** done, not just the direct children, because it makes an unqualified claim
  ("everything under here is finished") that direct-children-only could get wrong with an open
  grandchild. It is a prompt for the human to act on, never a status write (§6).
- **Nest / un-nest.** A picker offers every other row **minus the row's current container** and,
  since #1156, **minus every row the ladder would refuse as this row's container** — which is why
  a separate "promote to top level" option exists, sending the `parent`-clearing empty string,
  and why that option is itself withheld from a row whose level cannot sit at top level. This is
  deliberately a separate affordance from the existing dependency picker — containment is not
  ordering (§2), and conflating the two pickers would blur that. Still deliberately absent: any
  cycle/depth pre-filter. A row's own descendants are offered like any other row, exactly as the
  dependency picker offers a cycle-closing dep — that rule lives once, inside the backend's lock
  (§3), and its error names the path through this same picker's toast.

  **Why the ladder is mirrored when the cycle rule is not**, since this is the same reasoning
  reaching opposite conclusions. What made a client copy of the cycle and depth-cap rules a bad
  trade is that they are properties of the whole mutable tree, re-derived per candidate — a
  second implementation would have to walk the same graph and could disagree about any board.
  The ladder is a fixed table over two closed vocabularies that both sides already enumerate in
  full, so the copy is a lookup, not a re-derivation. It also earns more: on a levelled row an
  unfiltered nest picker is almost entirely illegal choices, which teaches the ladder one toast at
  a time. The backend stays the authority — every refusal still surfaces through the same toast —
  and the two tables are held together by **one** test: `the board's ladder table is the
  backend's, read out of the Rust source` (`test/taskboard.test.ts`) reads `ladder_rule`'s match
  arms out of the Rust source and compares them to the board's table, so editing either ladder
  alone reddens rather than being discovered by a user's refused write.

  **The two per-side table tests do not buy that, and the first draft of this note said they
  did.** `the_ladder_table_is_pinned_on_the_rust_side` and "the ladder table is the same one the
  backend enforces" each assert their own side against their own literals, so editing one
  language's rule *and its own test* leaves the other green and the ladders diverged. A pair of
  same-shaped tests either side of a language boundary reads like an equivalence and is not one —
  it catches a rule edited without its test, which is worth having and is a strictly weaker claim.
  The source-scanning guard is what makes the equivalence real, in the shape `tests/groupid.rs`
  and `test/perfpolicy.test.ts` already establish: default-deny, decided on a shape that cannot
  compile another way, blind spots stated in the test itself.
- **Set kind — landed later, slice K.** Everything above shipped with #1027; this one item
  followed in a later slice once the demo showed the badge but no way to change it. A third,
  separate picker (a `🏷` button, alongside `⤵` nest and `🔗` depends-on) — the same
  one-picker-at-a-time state (`PickerTarget`/`nextPicker`/`pickerIsOpen`) the nest and dep pickers
  already share, generalized from two fields to three.

  Since #1156 it offers the levels this row could **legally take where it sits**
  (`kindPickerChoices`), judged in both directions exactly as the backend judges the write: its
  container must accept the new level, and so must every row already inside it. Two consequences
  are worth stating because they change what a caller must handle. **The clear is no longer
  unconditional** — clearing the level of a row that holds levelled children would strand them, so
  it is withheld there. **The list can be empty**, which it never could when it was four-minus-one:
  a row whose container carries no level has no legal level at all, and the picker says so instead
  of offering a write that would be refused. An empty picker is a pointer at the fix (level the
  container first), which is the same job the refusal text does on the backend side.
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
- **Reading an ancestor's `status` in `ready`** — slice R folded ancestors' *deps* in (§6) and
  stopped there. The remaining clause ("a child of a container marked `blocked` is not ready
  either") is one line away and deliberately unshipped: `blocked` is this repo's status for
  blockers *outside* the board, so it is a claim about the container's own situation rather than
  about its subtree. It is the human's to opt into.
- **Role-template edits** — none were needed for this feature; the MCP tool descriptions are the
  teaching surface for the orchestrator, so no `pre222` fixture re-bless was owed by this work.
