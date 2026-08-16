# Human questions — asking without blocking the fleet

*#946. This note covers slice Q1: the registry, its MCP tools, and the trusted
answer surface — plus, from #1091, the shape of the ask itself (slice A), the
demo half of the same human-attention surface (slice B), the NEEDS-YOU panel and
its board deep-link (slices C and G, the Q2 surface), the derived attention
reason and opt-in toast (slice D), and the protocol prose plus the liaison's
pose gate (slice E). The structural deny of the blocking dialog (Q4) and the
chat bridge (#947 / T1) sit on top of what is described here and extend this
note as they land.*

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
| `select` | `single` (default) \| `multi` — how many options may be picked (#1091) |
| `allow_free_text` | bool, **default `true`** — may the human type their own answer (#1091) |
| `status` | `pending` \| `answered` \| `withdrawn` |
| `created_ms` | |
| `answer`, `settled_by`, `settled_ms` | present once settled |

**`urgency` is carried and rendered, but does not key either the attention item
or the toast.** The NEEDS-YOU panel (slice C) reads it to flag a question's own
card with a red "urgent" tag; the derived `question` attention reason (slice D)
and the opt-in toast do not branch on it at all — every pending question raises
the same non-urgent amber `question` chip regardless of `urgency`, the same
posture `gate` already has. It is in the schema from the first slice because the
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

### The ask shape (#1091 slice A)

What the human is *offered*, as opposed to what they are asked. Three additive
pieces, mirroring the affordance a CLI's own question dialog has and this one
did not:

| piece | shape | default |
| --- | --- | --- |
| option items | `"ship it"` **or** `{"label": "ship it", "description": "one review, bigger diff"}` | — |
| `select` | `single` \| `multi` | `single` |
| `allow_free_text` | bool | **`true`** |

`OptionSpec` is an untagged enum (`Plain` \| `Detailed`), and **a
description-less option is normalized back to `Plain` before it is stored**,
whichever form it arrived in. Both halves earn their place: untagged is what
makes a Q1-era file — where every option was a bare string — parse with no
migration; normalizing on the way in is what stops a richer build silently
restyling every file it touches, so the object form appears on disk exactly
where a description was actually given.

**`allow_free_text` defaults to `true`, and that is the load-bearing default.**
The options are the alternatives the *orchestrator* thought of; the answer
worth having is often the one it did not list, so denying the free-text box is
an explicit opt-out rather than something an ask can fall into. Two
consequences follow, and both are deliberate: a Q1 row with no such field reads
as `true` (free text was the only answer surface those rows were ever written
for), and `AskRequest::default` is hand-written rather than derived, because
`bool`'s derived `false` is precisely the value this field must never acquire
by accident.

**`select` and `allow_free_text` describe a list of options, so an ask that
gives either without one is refused** rather than absorbed. Each says the
orchestrator believed it was shaping a choice the human would be offered;
storing them silently would leave that belief uncorrected, and — for
`allow_free_text: false` with no options — would register a question with
nothing at all to answer it with. For the same reason `select` is parsed with
`Urgency::parse`'s posture: an unrecognized value is an error, never a
defaulted `single`, because an orchestrator that wrote `"multiple"` meant the
human to be able to pick several.

Bounds follow `validate_ask`'s existing refuse-never-truncate rule:
`OPTIONS_MAX` 8 unchanged, label `OPTION_TEXT_MAX` 200 unchanged, description
`OPTION_DESC_MAX` 500 (wider, because the description is where the trade-off
goes while the label stays short enough for a button). A truncated description
would cut exactly the part that decides the answer.

**Downgrade is not promised, and the asymmetry is the point.** An old build
reading a new file that carries an object option fails its parse *loudly* — the
posture the "reads are loud about a bad file" rule above argues for — rather
than dropping the row. Losing a pending question a human has not answered is
the one failure this registry exists to prevent, so a refusal a human can see
beats a silent read that a subsequent write would make permanent.

**A description is agent-authored text on its way to a trusted surface.** Up to
8 × 500 characters, written by an orchestrator, persisted, and rendered by the
NEEDS-YOU panel — which is loomux's own webview. It is stored, never executed,
and bounded on both axes, so the registry's job is done; the obligation lands on
the renderer, and it is the same one every other agent-authored string in this
app carries: **labels and descriptions are set as text nodes, never as
`innerHTML`.** Stated here rather than left implicit because this slice is the
first to route agent text onto that surface, and the panel that renders it is
written in a different slice by a different worker.

Both `select` and `allow_free_text` serialize **unconditionally**, following
`urgency` and `status` rather than `options` and `task`: the
`skip_serializing_if` fields are the ones that can be genuinely absent, while
these two always carry a value that decides how the row may be answered. A
pending question is a record a human may read straight out of the file, so it
says what it means instead of requiring the reader to know the defaults.

The answer stays **one string**. A surface composes it from the selection and
the free text; nothing structured is persisted beside it, because the consumer
is an LLM reading a pane notice, and labels quoted verbatim are unambiguous
there while a `selected: string[]` would be permanent schema for a fidelity the
notice and the audit line already carry.

### MCP tools

| tool | tier |
| --- | --- |
| `ask_human(text, options?, select?, allow_free_text?, task?, urgency?)` | orchestrator-only |
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
  *then-planned* latched attention reason and the opt-in toast are Q2. Until
  then a registered question is visible only in `questions.json`, in the audit
  log, and to agents through `list_questions` — the trusted answer command
  exists (`orch_question_answer`) but nothing in the UI calls it. *(Delivered
  by #1091 slices C and D, below — this paragraph describes the state at Q1's
  merge and is left as the record of it. The attention reason shipped
  RE-DERIVED every scan, not latched — see the slice C/D/G section below.)*
- **No role template teaches the protocol.** Q3 rewrites the orchestrator's
  open-question invariant — mark the task blocked citing `q-N`, re-surface from
  `list_questions`, un-block only that task on the answer. Until it lands, an
  orchestrator's use of `ask_human` is guided by the tool description alone.
  *(Delivered by #1091 slice E, below — this paragraph describes the state at
  Q1's merge and is left as the record of it.)*
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
for (slice C) needed no new read surface either.

**What slice B did not build, at the time it landed.** No UI rendered
`demo_path` — a recorded path was visible only in `tasks.json`, in the audit
log, and to an MCP caller of `get_task` (never `list_tasks`, which deliberately
omits it, above). That gap closed with the NEEDS-YOU panel's DEMOS section
(slice C) and the task-board marker + deep-link (slice G) — both shipped; see
the slice C/D/G section below.

### Slice E: the protocol lives in the contract, and the liaison can pose

Two changes, and they are one argument seen from each end of the pane pair that
faces the human.

**The orchestrator's half — a contract edit, not a tool-doc one.** Q1 shipped
`ask_human` with a description written in the imperative and no template prose
at all. That is the weakest place the rule could live: a tool description is
read once, at listing time, and is among the first things a summary drops. The
failure this feature exists to prevent is not "asked badly" — it is a CLI's own
blocking question dialog holding the pane, which makes it take **no delivery at
all**, so the stall is fleet-wide (#946). So `orchestrator.md` gains an
**Asking the human** section carrying the prohibition, the consequence that
makes it make sense, the six-step protocol and the authoring rules; INVARIANT 2
gains the one sentence that survives a compaction; **Durability rules** adds
`list_questions()` to the session-start reconcile, since a pending question
outlives the process where a registered notification does not. Unconditional,
never behind `{{WORKFLOW}}` — a group with no custom roster is exactly the group
that would otherwise still be free to stall its own fleet. Re-blessed in the
same commit (`tests/fixtures/pre222/README.md`).

**The liaison's half — the pose gate widens.** `role_hint: liaison` blocks are
`Role::Reviewer`, so before this the human's own pane could not call `ask_human`
at all. Its only durable route for "the human should decide this later" was
`message_orchestrator`, which becomes a registry row **only if the orchestrator
independently chooses to open one** — orchestrator-controlled, and therefore not
a path the human-facing pane has. `ask_human`'s dispatch gate and its listing
therefore move from `require_orchestrator` to `require_orchestrator_or_liaison`,
the same helper `group_usage` already uses (#891 S2), keyed on the same
conjunction (`kind: reviewer` **and** `role_hint: liaison`) and reading a hint
that is resolved from the group's roster rather than from anything a caller
supplied.

**What did NOT widen, and why each stayed put:**

- **`withdraw_question` is still orchestrator-only.** Withdrawing *settles* a
  row — any pending row, not only your own. The widening bought the human's pane
  the ability to **add** to their inbox, never to decide what leaves it. A
  liaison whose question is overtaken by events says so with
  `message_orchestrator`, and the orchestrator withdraws.
- **Nothing can answer one.** The trust boundary at the top of this note is
  untouched: no answer tool exists, `AnswerSource` stays a closed enum supplied
  by the entry point, and the widened tool is on the ASK side of a boundary that
  was always drawn between asking and answering rather than between roles.
- **The answer notice still goes to the orchestrator.** `answer_question`
  delivers through `deliver_to_orchestrator` regardless of who asked, and this
  slice deliberately does not change that: an answer's consequence is
  un-blocking a board row, and only the orchestrator writes the board. The
  asymmetry is stated in three places the reader will actually hit it — the
  tool description, the liaison's own mechanics fragment ("`list_questions` is
  how you see what became of yours") and the orchestrator's `{{LIAISON_NOTE}}`
  ("`list_questions` will show questions you did not ask — read the `asker`").
  **The ROUTING itself** is pinned by
  `a_liaison_block_may_pose_a_question_to_the_human` (direction 4), so a future
  change to it reddens rather than quietly making those three surfaces false;
  the prose surfaces are not each individually pinned, and saying so is the
  difference between a coverage claim and coverage. The tool reply is the one
  of them that IS asserted, since it is read at the moment the pane acts.

**Two run-time strings are branched on the caller, and that is the same defect
class twice.** Widening a gate makes every message on the widened path reachable
by a caller it was not written for. `require_orchestrator_or_liaison` therefore
takes the refused capability in words rather than hard-coding one caller's tool
name; `ask_human`'s SUCCESS reply branches on `caller_is_liaison`, because the
orchestrator's version tells the caller to mark a board row (a liaison has no
tool for it) and to expect the answer notice in its own pane (it will not
arrive there); and `PENDING_MAX`'s refusal names `withdraw_question` *or* the
orchestrator, rather than a tool half its callers have not got. The predicate
itself lives in one function so the gate and the reply cannot disagree — "the
gate said liaison, the reply said orchestrator" is exactly the asymmetry
CLAUDE.md's guard convention names. The reply branch is pinned both ways
(`a_liaison_block_may_pose_a_question_to_the_human`, step 1a plus its positive
control): the liaison's reply must not carry the two orchestrator clauses, and
the orchestrator's must still carry them — an assertion that only checked the
first would pass on a build that deleted the guidance for everyone.

The capability argument for the grant itself, and why the "it only reads"
reasoning that carried `group_usage` does **not** carry a write, is in
`doc/design/liaison.md` — that note owns the enumeration of every hint-keyed
exception, and a rule invisible there is the surprise it exists to prevent.

### Slices C, D, G: the NEEDS-YOU panel, the derived attention reason, and the board deep-link

The human's half Q1 left open (above) is now shipped, in three pieces that stay
disjoint on purpose (plan-783's D-vs-H conflict-avoidance note): a presenter
(C), a badge (D), and a board-side pointer at both (G). None of them adds a
Tauri command, a registry, or a second copy of a status set — each is wiring
over what Q1/slice B already persisted.

**Slice C — the panel.** A new `decisions` `EmbedKind` on orchestrator panes
(`Alt+Q`), built the same way every other embed is: an overlay/flex-slot pane
feature, never a PTY resize (constraint 1). It reads `orch_questions_list` and
`orch_tasks` and writes through the commands that already existed —
`orch_question_answer` for a decision, `orch_proceed_task`/
`orch_request_changes`/`orch_upsert_task` for a demo (the same calls the task
board's own buttons make) — so the panel and the board can never disagree about
either record. Two projections, `decisions.ts`'s pure core: pending questions
(unanswered) plus a faded settled tail capped at `SETTLED_SHOWN` (10, mirroring
`LIST_SETTLED_CAP`), and demo-gated board rows (`prototype`/`human-testing`),
each carrying whichever `demo_path`/`pr` the board holds. Answering composes the
chosen option labels (verbatim) and any free text into the one string
`answer_question` accepts — no structured `selected[]`, per the "answer stays
one string" rule above.

The panel-to-board direction (a question's card links to the `task` it cites)
and the board-to-panel direction slice G adds both ride one generic mechanism,
`pane.ts`'s `requestEmbedFocus(kind, target)` over `embedfocus.ts`'s
`PendingEmbedFocus`: it parks a target id — one slot per embed kind, replacing
any undrained request for that kind rather than queueing — and lazily
constructs/opens the named embed, which drains the request (`take`, once) on
its own next render, since every embed is lazily constructed and renders from
an async refresh, so the target row may not exist yet at request time. Built
once in slice C because both directions need it and it is intra-pane wiring,
not a new command — see `src/embedfocus.ts` and `doc/design/embedded-panels.md`
for the surrounding embed-engine contract it rides.

**Slice D — the derived attention reason.** The 3-second attention scan gains a
`question` reason: any agent with a pending question it asked (`q.asker`,
counted from `questions.json`) gets a non-urgent amber chip, "N pending
question(s)" — ranked below `blocked`/`stranded`/`waiting`/`report` and above
`gate` in the scan's priority order, and re-derived every tick rather than
latched, so it clears the instant the last pending row is answered or
withdrawn. It rides the existing per-group opt-in desktop-toast path
(`attention_toast_targets`) for free — that path already fires for every
reason **except** `gate`, so a fresh question toasts exactly like a
blocked/report event once a group opts in, while a task merely reaching a merge
or demo gate (too common to alert on — every PR does it) still does not. See
the `urgency` correction above: this reason and its toast do not read
`urgency` at all.

**Slice G — the board marker + deep-link.** `taskboard.ts` derives, never
stores, two signals per row: **decision-blocked** — a pending question whose
`task` names this row (`blockedTaskMap`, built once per board render from the
pending-questions list) — and **demo-gated** — `isDemoGated`, the same status
set (`prototype`/`human-testing`) the panel's own DEMOS tier uses, moved here
from `decisions.ts` and re-exported so neither module holds its own copy.
`boardMarker` projects at most one chip per row: decision wins when a row is
somehow both, because it is the more specific, more blocking ask. The chip
routes through the same focus hook slice C built, opening the NEEDS-YOU panel
at the citing question (a decision chip) or at the row's own card (a demo
chip).
