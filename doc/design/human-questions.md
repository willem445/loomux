# Human questions — asking without blocking the fleet

*#946. This note covers slice Q1: the registry, its MCP tools, and the trusted
answer surface. The inbox UI (Q2), the orchestrator's protocol prose (Q3), the
structural deny of the blocking dialog (Q4) and the chat bridge (#947 / T1) sit
on top of what is described here and extend this note as they land.*

## The problem

A question the orchestrator asks the human is, mechanically, a modal dialog in
its own CLI. While that dialog is on screen the pane cannot take **any**
delivery — loomux already knows this and correctly withholds pastes
(`HeldReason::InteractiveQuestion`, the "held: question pending" badge) — so
every worker report, review verdict and merge request queues behind it, eight
deep, and then starts being refused.

The consequence is not "the orchestrator waits". It is that **machine progress
stops on human absence**. One clarifying question asked while nobody was at the
keyboard held a whole run overnight: the in-flight workers finished their PRs
and then nothing was reviewed, dispatched or merged until morning.

So the requirement is narrow and absolute: *the orchestrator must never enter a
state where human absence stops machine progress.*

## The shape, and the one that was rejected

**The pending question is engine state. Any presenter is a client of it.**

`questions.json` lives in the group directory beside `tasks.json` and
`state.json`. `ask_human` appends to it and returns an id immediately;
`answer_question` settles a row and delivers a notice; nothing anywhere waits.

The literal reading of #946 — a dedicated liaison agent that holds the pending
question and relays it — was rejected because it re-creates the incident one
level up. An agent pane is an LLM session: it compacts, it wedges, it dies —
and exempting one from the idle reaper, as a liaison now is (#891 S4), changes
none of those. Putting the thing that un-blocks the fleet inside one means a
wedged liaison is a deaf fleet, and the failure is harder to see than the one it
replaced. A liaison may still *present* questions and relay context (that is
#891's job, and it composes with this); it simply is not the record.

This is also what #888 needs. Nothing in the registry references the webview: a
headless engine keeps the same file, the same tools and the same audit, and the
answer surfaces become protocol commands rather than IPC ones.

## The trust boundary

**Every agent may ASK. No agent may ever ANSWER.**

An answer settles a question the *human* was asked and releases the work waiting
on it. An agent that could produce one would be answering its own gate — the
same self-served-gate shape CLAUDE.md constraint 9 refuses for install and
security prompts — and the feature would be theatre rather than a mechanism.

Three layers hold it, and none of them is a convention a caller can opt out of:

1. **No answer tool exists.** `mcp.rs`'s `call_tool` is a closed match on tool
   names, and no arm of it reaches `OrchRegistry::answer_question`. An agent
   cannot call what has no name.
2. **The source is a property of the entry point, never an argument.**
   `AnswerSource` is a closed enum whose variants are trusted surfaces, and
   `orch_question_answer` hard-codes `AnswerSource::Webview` rather than
   accepting a `source` string. "Answer as someone else" has no spelling, so
   there is nothing to validate and nothing to forge.
3. **Provenance is durable.** Every settle and every refusal is audited with its
   source tag, so who answered — and what was turned away — is reconstructable
   from the log.

Two tests keep this true as the code grows, and each carries a deliberate
guard against proving nothing:

- `no_agent_token_can_answer_a_question_through_the_mcp_surface` drives every
  tool both roles are offered, plus the names a future slice might plausibly
  give an answer tool, and asserts the question still carries no answer. It
  ends with a **positive control**: an answer entered through the trusted path,
  asserted to land. Without it the sweep would pass for two indistinguishable
  reasons — the boundary held, or nothing was ever observable — and would keep
  passing even if the status field stopped being written.
- `the_mcp_surface_has_no_path_to_the_answer_entry_point` scans the source: no
  call to the entry point and no mention of `AnswerSource` in `mcp.rs`, the
  type confined to its two homes, and — the part that pins the boundary rather
  than the filing — **the closed set of `AnswerSource` variants**, read off the
  declaration itself. `AnswerSource::Agent` added inside `humanq.rs` is
  invisible to a "where may it be named" check and would hand an agent the
  power this whole section exists to withhold; the set assertion is what
  catches it.

**Adding an answering surface** (the #947 chat bridge is the planned one): add
an `AnswerSource` variant and a trusted entry point that supplies it. Never a
`source` parameter, and never an MCP tool.

The one settle an agent *can* perform is `withdraw_question`, and it is
deliberately the settle that produces no answer: an orchestrator taking back its
own overtaken question is not the same power as deciding it.

## Public contracts this ships

### `questions.json`

An array of question records in the group dir. Every field past the required
core carries `#[serde(default)]`, so a file written by an older build loads.

| field | notes |
| --- | --- |
| `id` | `q-1`, `q-2`, … |
| `asker` | agent id that asked |
| `text`, `options`, `task`, `urgency` | as asked; `options` omitted when empty |
| `status` | `pending` \| `answered` \| `withdrawn` |
| `created_ms` | |
| `answer`, `settled_by`, `settled_ms` | present once settled |

`urgency` is carried but not yet acted on — Q2 keys the attention item and the
opt-in toast off it. It is in the schema from the first slice because the
persisted shape is a contract, and adding a field later means migrating files
that are already holding questions a human has not answered.

**Ids are legible, not opaque.** `q-{highest + 1}`, read off the file exactly as
`upsert_task` mints `t-N`. Constraint 2 (no getrandom-based crates) is satisfied
either way — no crate is involved — and unpredictability buys nothing here: the
id is never a capability, since the only surfaces that can act on it are trusted
ones, and it *is* quoted in a board note, in the answer notice and (from #947) in
a chat message, where legibility is load-bearing. Ids are never reused: retention
only ever drops rows below the high-water mark that produced them.

**Reads are loud about a bad file.** `OrchRegistry::questions` treats an absent
file as empty and *every other failure as an error* — deliberately unlike
`tasks()`, which collapses all of them to an empty board. Every mutation is a
read-modify-write of the whole file, so a read that answered "no questions" for a
file it merely failed to parse would let the very next `ask_human` overwrite it,
destroying pending questions a human has not answered. That is the one loss this
registry exists to prevent. (`orch_questions_list`, which only reads, still shows
an empty list — it has no error channel and its caller renders a list.)

**Retention.** Settled rows are capped in the file at `SETTLED_RETAINED`, oldest
out first; the audit log keeps all of them regardless. Pending rows are *never*
pruned at any count — `PENDING_MAX` bounds them by refusing new asks instead,
loudly, because reaching it means questions are being asked faster than any human
could answer.

### MCP tools

| tool | tier |
| --- | --- |
| `ask_human(text, options?, task?, urgency?)` | orchestrator-only |
| `withdraw_question(id)` | orchestrator-only |
| `list_questions()` | shared read |

Orchestrator-only for the writes because delegates already have
`message_orchestrator`: one funnel to the human, one authoring standard for what
a human is asked. The reads are shared on purpose — a delegate that can see a
question it depends on is already outstanding is the opposite of a leak, and it
stops the same question being raised twice. Both halves of the #243 double gate
apply: the role-filtered listing is cosmetic, and `call_tool`'s check is the
gate.

### Tauri commands

`orch_questions_list(group_id)` (orch-read) and
`orch_question_answer(group_id, id, answer)` (orch-control). Both parse
`group_id` at the boundary through `command_group`, like every sibling command.

`orch_questions_list` returns the **whole file**, deliberately not the capped
`list_questions` projection: that cap buys an agent context economy, and this
command's list-typed return has nowhere to carry the omitted count that keeps
the cap honest. Retention already bounds the file, so uncapped is a bounded
answer here.
Membership is enforced by *which file was read*: each group's questions live in
its own group dir, so another group's id is simply absent, and the refusal is the
same one an id that never existed gets.

### The answer notice

`[loomux] answer to q-N (via <source>): <answer>`, delivered through the ordinary
`deliver_to_orchestrator` path — the same queue every other `[loomux]` notice
uses, so the human sees it in the pane verbatim.

The answer is untrusted text entering a `[loomux]` line. The human is trusted;
the pane still cannot tell one line from another, so an embedded newline would
forge a second line that reads as its own legitimate notice. It goes through
`sanitize_gh_text` like every other notice field, and the id and source tag —
which loomux builds — are emitted before it so the cap trims the answer's tail
rather than the attribution.

**A delivery failure never fails the answer.** No live orchestrator, a full pane
queue, a restart mid-answer: the question is settled durably either way, and a
cold orchestrator finds it through `list_questions`. That the registry is the
record and the notice only a notification is the whole design in one sentence.

### Audit actions

`question-open`, `question-answer`, `question-withdraw`, `question-reject`. The
reject line carries the reason (`unknown-question`, `already-settled`,
`invalid-answer`) and the source, which is what makes a probe visible rather
than merely refused.

## What Q1 does and does not reach — state at merge

**The tools are live and advertised from the moment this merges.** `ask_human`,
`list_questions` and `withdraw_question` are in `tool_defs`, so every
orchestrator in every group is offered them, with descriptions written in the
imperative ("USE THIS INSTEAD OF YOUR CLI'S OWN INTERACTIVE QUESTION DIALOG").
An orchestrator reading its own tool list will find `ask_human` and may call it
on day one. Nothing about this slice is inert, and it should not be described as
though it were.

What is **not** here is the human's half:

- **No surface shows the human a pending question.** The inbox panel, the
  latched attention reason and the opt-in toast are Q2. Until then a registered
  question is visible only in `questions.json`, in the audit log, and to agents
  through `list_questions` — the trusted answer command exists
  (`orch_question_answer`) but nothing in the UI calls it.
- **No role template teaches the protocol.** Q3 rewrites the orchestrator's
  open-question invariant — mark the task blocked citing `q-N`, re-surface from
  `list_questions`, un-block only that task on the answer. Until it lands, an
  orchestrator's use of `ask_human` is guided by the tool description alone.
- **The blocking dialog is still available.** Q4 adds it to the orchestrator's
  CLI deny tier, which is what makes the original incident impossible rather
  than merely discouraged.

The answer *delivery* is wired here: `answer_question` sends the `[loomux]`
notice through `deliver_to_orchestrator`, so an answer that is somehow entered
does reach the pane. The gap is upstream of that — with no inbox, there is in
practice **no human-answer path** for a question asked between this merge and
Q2, and such a question will sit pending until Q2 surfaces it or the
orchestrator withdraws it.

That is acceptable only because of the property the whole design is built on:
asking never blocks. A question with no answer path costs a pending row and an
unresolved decision — not a stalled fleet. It is still the reason Q2 should
follow closely rather than eventually.

## #1091: one human-attention surface, questions plus demos

#1091 folds a second kind of "needs the human" item — a **demo item** — into
the same NEEDS-YOU panel this registry feeds, per the human's demo-tracking
scope addition on the issue. A demo item is deliberately **not** a second
registry: it is a projection of the existing task board, in a demo-gated
status (`prototype` or `human-testing`). The record stays `tasks.json`; this
registry's `questions.json` is untouched by it. See #582/#958's `Task`
doc-comments for the rest of the board's own contract — this section covers
only what #1091 slice B adds to it.

### `Task.demo_path` (slice B)

One additive, optional field: `demo_path: Option<String>` — the worktree path
where a demo of that task lives, e.g. `C:/Projects/loomux-worktrees/feat/x`.
Same `#[serde(default, skip_serializing_if = "Option::is_none")]` contract as
`parent`/`kind` (#958) — **not** every optional `Task` field: `pr`/`pr_base`/
`assignee`/`session`/`issue` carry only `#[serde(default)]` and write an
explicit `null` when absent, while `parent`/`kind`/`demo_path` omit the key
entirely. Either way a pre-#1091 `tasks.json` loads with the key simply
absent, and a board that never sets `demo_path` rewrites without gaining the
key (pinned the same way #958 pins it for `parent`/`kind`: a rewrite of an
untouched board must not gain the key).

**Explicit beats inferred.** The alternative — deriving a demo location from
the assignee's roster row (its `cwd`) — was rejected: the orchestrator
preparing a demo often does so from an integration-branch worktree that no
single worker's `cwd` names, and inferring would need a new read command plus
its own ACL and perf-manifest entry to serve a worse answer than the
orchestrator just recording the path it knows. `demo_path` is DISPLAY METADATA
ONLY, the same posture as `pr_base` (#581): nothing gates on it, and a stale or
wrong value misleads a human rather than opening anything.

Set through the same two surfaces every other task field goes through — no new
tool, no new command:

- **MCP** `upsert_task(..., demo_path?)` — the orchestrator's write path,
  same untouched/empty-clears rule as `pr`/`pr_base` (omit to leave it,
  `""` to clear).
- **Tauri** `orch_upsert_task(..., demo_path?)` — the human board's own edit
  path, additive like the `parent`/`kind` args #958 added before it.

Deliberately **not** added to `TaskSummary` — the compact row `list_tasks`
returns (#245's size constraint: that row stays minimal by construction, and
slice B's plan never asked to widen it). A caller that needs
`demo_path` reads the full record: `get_task` (MCP) or `orch_tasks` (the human
board's own Tauri read, already full `Task`s) — so the panel this field exists
for (slice C, not yet built) needs no new read surface either.

**What slice B does not build.** No UI renders `demo_path` yet — that is the
NEEDS-YOU panel's DEMOS section (slice C) and the task-board marker + deep-link
(slice G). Until either lands, a recorded `demo_path` is visible only in
`tasks.json`, in the audit log, and to an MCP caller of `get_task` — **never**
`list_tasks`, which deliberately omits it (above) — reachable, not yet
surfaced.
