# Design: what a polled read is allowed to carry

Status: implemented (issue #1317).

Scope: the three wire shapes #1317 changed, and the one rule they are three
instances of. The retention question #1317 declined to answer is #1472, and it
is not addressed here.

## The rule

> **A polled read's payload is bounded by how much work is LIVE, never by how
> long the session has been running.**

`performance.md`'s INV-3 and INV-4 bound how *often* the webview pays, and
INV-8 bounds what a handler *retains*. Neither says anything about how big one
tick is, and #1317 is the gap between them: three reads whose per-tick size was
a direct function of session length, on a fixed cadence.

None of them was a retention leak — each is replaced wholesale every tick, so
nothing accumulates. What grows is the **size of one tick**: allocation churn
proportional to how long the human has been running, which is the shape a long
session feels as GC pressure and, at the tail, as an OOM it did not cause on
its own (#1301).

Two corollaries, both of which the three sections below lean on:

- **A payload with no reader is not free.** Each of these shipped rows that the
  consuming view provably never looked at. The fix in every case was to find
  out what the reader actually indexes, not to pick a cap.
- **A fold is not a truncation if the reader can see it.** Every cut below
  keeps the whole-population TOTALS, names the population's size, or both — and
  RENAMES the key it narrowed, so a reader written against the old shape fails
  loudly instead of quietly rendering a subset. `mcp::summarize_group_usage`'s
  `rest` count (#866) is the precedent for all three.

## 1. `orch_group_usage`: `agents` → `live_agents` + `agent_count`

`compute_group_usage` merges each live agent's fresh snapshot with every
snapshot `mark_dead` ever captured, so its `agents` array is O(agents-EVER).
The group view rebuilt and re-indexed it into a fresh `Map` every 2 s; the tab
bar polled the same payload every 4 s per group-bound tab. The MCP twin had
been capped for exactly this (a 654-agent roster measured at 173,245 chars);
the command the GUI polls had not.

The GUI never wanted the historical rows. `groupview.ts` indexes the array by
agent id and looks up only the agents `orch_group_summary` reports **live**;
`tabbar.ts` reads `live_cost_usd` and no row at all. Live agents are bounded by
the group's `max_agents`; the lifetime roster is bounded by nothing.

**Why live, and not a top-N cap like the MCP twin's.** Top-N there is chosen by
lifetime tokens, which on a long-history group can push every live agent out of
the list — the MCP side handles that with `rest.live`. For this reader the live
rows are not *most* of what it wants, they are *all* of it, so a cap would be
both looser and capable of dropping the only row the view renders.

**What keeps it honest.** Every `lifetime_*` total still sums the whole roster,
so nothing leaves the figure on screen; `agent_count` names the roster's real
size, so `agent_count != live_agents.len()` is readable.

**One computation, two projections.** The per-group memo cell (#743 S4b) holds
the full value and its live projection together under one `Instant`. Deriving
the projection per caller instead would have moved the O(roster) clone from the
webview to the backend rather than removing it; two memo cells would let the
two drift onto different windows. `OrchRegistry::group_usage` — the MCP tool,
the autonomy anchor, the budget enforcer — is unchanged and still gets the
whole roster.

## 2. `orch_tasks`: note bodies ride only for the rows asked for

`orch_tasks` returns the whole board, is polled, and is re-fired by every
`orch-tasks-changed` event — so an agent's write burst multiplies it by the
number of open boards. Text *within* a row is capped (`MAX_TASK_NOTES` = 20);
the row count is not, and a long-lived group's board is mostly history (400+
rows, nearly all `done`). 400 rows × up to 20 notes of prose is an order of
magnitude more wire than every other field on the board put together.

The board reads those bodies in exactly one place: the notes list under a row
the human has **expanded**. Everywhere else it reads a count, for the `🗨 N`
badge. So the bodies ride only for the rows named in `with_notes`, and every
row carries `note_count`. That is the split MCP's `list_tasks`/`get_task` pair
already draws for the agent side, arrived at from the other direction.

**`Option`, not an empty vec.** `None` means "you did not ask for this row's
bodies"; `Some([])` means "you did, and it has none". Collapsing them would
render a row whose bodies are still in flight as one whose conversation had
been deleted — so the panel says *loading notes…* for the one tick between an
expand and the read it triggers, and only for a row whose `note_count` says
there is something to load.

**`BoardTask` stopped flattening `Task`.** It is now an explicit projection
with an exhaustive destructure, following `AgentTaskView` and for its argument:
a `#[serde(flatten)]` of the storage type hands every future `Task` field to
this wire the moment somebody adds one. The compiler now asks instead.

**What this does NOT fix.** The row COUNT is untouched and still O(session).
That is not an oversight and it is not liftable by a payload change. `TasksView`
reads the whole board at 54 sites, feeding it to 32 distinct pure helpers, and
several of those structurally need the rows the renderer HIDES — which a
server-side row cap would take away rather than merely leave unrendered. The
code states the requirement itself in two places worth quoting:

- `closedSubtrees`, which both `clearedIds` and `settledIds` are built on,
  decides a row's fate with `pred(t) && (children.get(t.id) ?? []).every(holds)`,
  so whether a VISIBLE row is archived depends on every descendant — hidden
  ones included. `clearedIds`' own doc names the consequence: a cleared
  container holding a live child stays on the board, because it is the only
  thing on screen that says where that child lives.
- `visibleRows` WALKS archived subtrees rather than pruning them, so their rows
  are marked seen and cannot resurface as stray top-level rows.

Bounding the row count is therefore a **retention** decision — what may leave
`tasks.json`, on whose authority, and how a human gets it back — not a payload
one. It is #1472, and it wants a human's answer before it wants code.

## 3. `orch_needs_you_list` carries the rows its open items name

The NEEDS-YOU panel fetched the whole board beside its items, every tick and on
every `orch-tasks-changed`, and used it at one site: `linkTask` looks up the
row an OPEN item names and projects six identity/status fields off it. So the
panel held a second full copy of that mostly-historical board to answer a
handful of point lookups — the other half of #1317's item 2, and the half that
*is* liftable without a retention decision.

The read now carries one row per distinct task an OPEN item names, and no
others. `items` is bounded by `needsyou::OPEN_MAX` and each item names at most
one row, so the join is bounded by the human's own queue.

**Open-only is deliberate.** The settled tail renders from the item's own
record and never joins the board; joining it would put the board's growth back
on this read through the retained-resolved cap.

**It also closes a smaller correctness gap.** The panel's items and watermark
came from one read specifically so it could not render this second's rows
against last second's watermark — but the board arrived from a *separate*
parse of a file an agent can rewrite in between. Now all three come from one
read, and `projectPanel` takes the rows off the view rather than as a
parameter, which makes "these came from one read" structural instead of a
convention every call site has to keep.

## 4. The audit log: one read per pane, one extraction per read

Not a wire-shape change — the payload was already bounded by
`AUDIT_VIEW_LIMIT` (5000). What was unbounded was how many copies of it the
pane held and how often the derived form was rebuilt.

Two views read `orch_audit`: the audit viewer and the progress timeline. Each
fetched independently, each on its own 1.5 s follow tick, each held its own
5000-row copy — and audit `detail` is where the app's biggest strings live (a
`prompt` detail carries the whole delivered prompt). `src/auditstore.ts` is now
the single owner of that read, on `SessionStore`'s pattern (#493) and for its
reason: the answer the second caller needs is the one already being computed.

**Why it adds a freshness window to that pattern.** Single-flight alone does
not collapse two views following the same log at the same cadence — their ticks
rarely land inside one another. `AUDIT_READ_MAX_AGE_MS` (1200 ms, strictly
under the 1500 ms follow cadence so a view's own tick is never served its own
previous read) is what makes the second view's tick free. It is
`USAGE_POLL_MAX_AGE`'s shape, applied on the frontend because this read has no
backend memo. An explicit gesture — opening the panel, the ⟳ button, returning
to a visible window — passes 0 and is never served a cached answer.

**The third copy was derived, and it was rebuilt on the wrong event.**
`extractTimeline` builds one `TimelineEvent` per audit row — a freshly-composed
label and an alias of the raw `detail` blob, up to 5000 of them — and it ran on
every RENDER, *above* the no-op signature check and therefore even on the
renders that check then discarded: a window-preset click, a category chip, a
`ResizeObserver` tick, a dot click, expanding a detail row. It is now memoised
on the identity of its two inputs, so it runs once per read.

**Identity, not a content signature.** Both inputs are replaced wholesale
rather than mutated, so `===` is exactly "the same read". A content signature
would additionally skip the re-extraction on a tick where the log did not grow;
that is deliberately not taken, because "same length and same last timestamp"
is only sound while the log is append-and-drop-front, which the rotation across
`audit.1.jsonl` makes a claim rather than an obvious fact.

**Failure posture.** The store keeps the last good rows on a failed read and
does not throw: both callers already rendered an unreadable log as an
empty/unchanged view rather than a broken one, and a shared store must not turn
one view's transient failure into the other's. It does not latch either — the
stamp is advanced only by a successful read, so "I could not look" never
becomes "there is nothing new" for the rest of the session.
