# The lead pane — a human-driven pane that opens orrerix panes

*#2519. **Slice A** (this note as it stands) is the capability class:
`Role::Lead`, its enumerated MCP surface, the root invariant behind a child's
`report`, and the guardrail exemptions. The launch path that mints a lead group
and pastes its kickoff is **slice B**; the launcher toggle, the pane chip and
the Agents-tab grouping are **slice C**. Each slice extends this note rather
than replacing it. What is not yet shipped is one list, at the end.*

Companion notes: [manager.md](manager.md) — the opposite pole, and the guarantee
this feature must not weaken; [workflows.md](workflows.md) for the block model
and why a workflow file can never name this class; [liaison.md](liaison.md) for
the `group_usage` widening this class inherits by construction.

## What it is

A **lead** is an ordinary agent pane — the human's CLI, the human's model, the
human typing into it — that has been given the fleet-control slice of the
orchestrator's MCP surface. When it would reach for its harness's own subagent
mechanism (Claude Code's `Agent` tool, opencode's `task`, Codex's forks), it
calls `spawn_agent` instead and gets a real orrerix pane: watchable, steerable,
resumable, and still there when the turn ends.

It is **not** an orchestrator. There is no task board, no review gate, no merge
queue, no issue duties, no autonomous idle tick, and no orchestrator anywhere in
its group — the human is the loop.

## Why a sixth class rather than a hint or a subtraction

The `Role` enum is the security spine of #222: a workflow file *selects* a
capability class and can never define one, so every structural guarantee keys on
a closed, hand-written list. Three shapes were available and two are ruled out
by that same rule.

**Not "an orchestrator with tools stripped."** `Role::Orchestrator` is a trust
root with keyed behaviours well beyond its tool list — the loomux-owned workflow
block, the autonomous idle tick and autonomy budget, the review driver, the "the
one orchestrator" lookup a report resolves through, `spawn_agent`'s "a group has
exactly one orchestrator" refusal, the orchestrator kickoff. Stripping tools via
a `role_hint` would make capability a function of *data*, which is precisely
what #222 forbids; `manager.md`'s "a class whose instructions say it has no
`report` could have dispatched one" is the failure that produces.

**Not a widened `Role::Solo`.** A solo pane's contract is *zero group-scoped
power*, pinned by
`solo_role_tool_surface_is_exactly_channel_send_and_channel_status` and
`solo_role_cannot_dispatch_any_group_scoped_tool`, and every existing
channel-tools pane already holds a solo token. Widening it would re-grant fleet
control to panes nobody opted in.

**So: a class.** `manager.md`'s own test for whether a new class is warranted is
whether what distinguishes it is *structural*, and here it is on five axes at
once. A lead holds fleet tools an orchestrator holds; it is **not** a delegate
(no `report`, and nothing above it to report to); it is **not** reaped, capped
or nagged; it **is** a legitimate mid-session delivery target, because receiving
its children's reports is the stated effect of the toggle the human turned on;
and it is the **root** of its own group. No hint expresses "receives reports,
may spawn, is nobody's delegate".

## The surface

Built as a **positive enumeration** on `Role::Manager`'s pattern: the shared
read tier is built first, narrowed to a named allow-list, then extended with
what the class is for. It is a filter, so it is **default-deny** — a tool added
to the shared tier by a later slice does not reach a lead unless someone puts
its name in that array and argues for it here.

It is spelled **twice** — once in `mcp::tool_defs` (the listing) and once in
`call_tool`'s dispatch gate (the enforcement) — and the duplication is
deliberate, for the #243 double-gate reason: a single shared constant would make
one edit move both. `the_gate_and_the_listing_agree_for_a_lead` asserts the two
are equal, in both directions.

### Granted

| Tool | Why |
| --- | --- |
| `spawn_agent` | The capability the toggle exists to grant. A **different definition** from the orchestrator's, not a shared one: that description is three screens of contract a lead is refused (reviewer and planner classes, board-task grounding, resume machinery), and a surface that keeps advertising a route the tool refuses is read on every turn, re-grounding included. |
| `send_prompt` | Drive a helper. Class-neutral wording, shared with the orchestrator's tier. |
| `get_output` | Read a helper's tail. This is the cost argument: a helper's output stays out of the lead's context until it asks. |
| `kill_agent`, `focus_agent`, `rename_agent` | Manage the panes it opened. Shared wording. |
| `list_agents` | Know what it has open. |
| `group_usage` | "What is this costing", answered in the pane the human is already asking in — the liaison's argument (#891 S2) inherited by construction: a read of an aggregate scoped to the caller's own group, settling nothing and writing nothing. Opted in at the arm, NOT by widening `require_orchestrator_or_liaison`, which also gates `ask_human`. |
| `channel_send`, `channel_status` | The cross-workspace channel. A human connects one by hand, on this pane; see the `notify_when` row for why that distinction decides it. |
| `note_directive`, `request_compact` | Self-scoped; neither reaches another pane. |

### Withheld, and why

| Tool(s) | Why |
| --- | --- |
| `report` | A lead is the **root**: there is nothing above it. `deliver_relayed_to_root` would resolve the lead itself as the recipient — a pane relaying its own status into its own transcript. Refused at the gate AND in its own arm, so the arm can say what to do instead (the human is in this pane). |
| `message_orchestrator` | A lead group has no orchestrator. |
| `upsert_task`, `remove_task`, `list_tasks`, `get_task` | No task board exists in a lead group. Listing them would advertise routes with nothing behind them. |
| `ask_human`, `request_attention`, `withdraw_*`, `list_questions`, `list_needs_you` | The human is **in** this pane. Every one of these exists to move a decision to a human who is somewhere else. |
| `review_verdict`, `list_verdicts` | No review gate. A verdict here would open nothing. |
| the merge queue, `drive_review*`, `read_playbook` | No merge queue, no driver, no orchestrator playbook. |
| `get_state` **and** `set_state` | Withheld as a PAIR, and the read is the one this slice took back (rev-final B1). `state.json` has exactly one writer in the tree — `OrchRegistry::set_state`, reached only from an MCP arm that is `require_orchestrator` — and a lead group has no orchestrator by design, so nothing in one can ever write that blob. A lead holding the read alone would get `"{}"` back on every call, forever: the same "advertise a route with nothing behind it" as the board rows above, in its silent form. Whoever wants a lead to hold durable state argues for the WRITE and the read follows it; `the_group_state_pair_is_granted_together_or_not_at_all` pins the pair rather than either tool. The compact-survival need this was first justified by is served by `note_directive`, which IS granted. |
| `notify_when`, `list_notifications`, `cancel_notification` | **v1 decision, revisit.** A fired watch is text typed into the pane at a moment the human did not choose, requested by the agent rather than by them. That is the line this table draws, and it is the line `channel_send` sits on the other side of: a channel is opened by a human's own gesture on this pane, a watch is registered by the agent itself. |
| `acquire_lock`, `release_lock`, `list_locks` | A lead group declares no resources — there is no workflow file to declare them in (see *Consent* below). |
| `message_manager`, `check_mail` | The manager's mailbox belongs to a class this group does not have. |
| `session_digest` | Process-hinted workers only; unchanged. |

## The root invariant, and the manager

A group has exactly **one root**, and which class it is says what kind of group
it is: `Role::Orchestrator` for an orchestration group, `Role::Lead` for one
minted by the toggle. `Role::is_root` is that question as one predicate, and
`OrchRegistry::deliver_relayed_to_root` — the old
`deliver_relayed_to_orchestrator`, generalised — is its one consumer. So a child
of a lead reports into the lead's pane by *exactly* the path a worker reports
into an orchestrator's: same scrub of the agent-authored half, same maskable
marking, same `Delivery::MidSession` admission, and the same #1958 rule that a
`progress` report is recorded rather than delivered. The `report` arm needs no
branch on which kind of group it is running in.

**`Role::Manager` is not a root, and that separation is load-bearing.**
`Role::is_root` and `Role::is_fixture` differ by exactly that class. A manager is
the human's pane too, and it is emphatically *not* a report target: the
no-injection guarantee refuses every mid-session delivery into one, which is the
whole of `manager.md`. If the two predicates were ever folded together — or one
derived from the other — the root lookup would start *addressing* reports at a
manager, which `deliver_prompt` would then refuse, correctly, but only after
routing a report at a pane that can never receive one.

**Nothing here weakens that guarantee, and here is the exact reason.** It is
enforced in `deliver_prompt` by
`a.role == Role::Manager && !delivery.permitted_into_manager_pane()`. This slice
edits neither operand: `Role::Manager` is untouched, and
`Delivery::permitted_into_manager_pane` still admits exactly the two kickoffs
and the post-compact re-grounding notice — pinned as a SET by
`exactly_three_delivery_kinds_may_enter_a_manager_pane`, so a fourth carve-out
fails a count.
`a_child_report_is_typed_into_the_lead_pane_and_refused_into_a_manager` drives
both poles from one fixture, in one test, because that asymmetry *is* the
property: two separate tests would each pass against a build where `is_root` had
been folded into `is_fixture`.

## Consent: how a lead differs from a workflow-declared class

A repo's `.orrerix/workflow.yml` is a **consent surface** — the launcher previews
the roster it declares, and the human agrees to it before the group runs (#222).
A lead group has no workflow file at all: it runs the built-in roster, so there
is no roster to preview and no consent moment to skip. What stands in for it is
the toggle itself, which the human flips on their own launcher for their own
pane.

That is why **`workflow::kind_from_str` has no `lead` arm**, and its absence —
not any check — is what makes three refusals structural:

1. A repo file cannot declare `kind: lead`, so a repo can never hand a pane
   fleet control. The parse rejects it as an unknown kind, never coercing it.
2. **No recursion.** `spawn_agent` parses its `kind` argument through that same
   function, so no agent can open a lead — a lead least of all. There is no arm
   in the spawn tool claiming to enforce this, deliberately: an arm there would
   be unreachable code taking credit for a refusal the vocabulary makes.
   `a_lead_may_spawn_a_worker_and_nothing_else` asserts *which* check said no,
   by the vocabulary the refusal quotes, so adding a `lead` arm to
   `kind_from_str` fails that test rather than quietly enabling recursion.
3. A `resume_session` carrying a recorded `role: "lead"` and no block id is
   refused rather than re-roled, by the same #544 "never guess a capability
   class" path every unrecognized role takes: the `block.trim().is_empty()`
   branch runs `kind_from_str` on the recorded role and errors on `None`. A
   recorded row that *does* carry a block id takes the other branch and is
   refused a step later instead, by the lead caller's own effective-class check
   (`declared.or(kind)` is then `None`). Both routes refuse; only the first is
   the vocabulary's doing, and which is which is worth spelling out — a review
   round turned on exactly that distinction (rev-final R1).

`Role::Solo` is absent from that vocabulary for the identical reason and is the
precedent.

## What a lead may open

**A worker, and nothing else.** Reviewer and planner are refused because neither
has anything to answer to here: there is no merge gate for a verdict to open and
no board for a plan to be recorded against, so both would arrive contained for a
contract nothing enforces. A worker briefed to review, or to investigate and
report back, is the same work with an honest posture. Orchestrator and manager
are refused by the two pre-existing argument checks, unchanged.

The check reads the **effective** class, below the block resolution and the
resume inheritance, and that position is the whole of it. The two manager
refusals in the same arm needed three checks between them precisely because a
block's kind *wins* over `kind:` at `spawn_agent_ex` — so a caller-class rule
written against the `kind` argument alone would have the same three-way hole
with one third of it plugged, and `kind: "worker", block: "reviewer"` would open
a reviewer. `a_lead_cannot_spell_around_the_worker_rule_with_a_block` is that
case.

## Guardrails

`Role::is_fixture` replaces seven hand-written copies of
`matches!(role, Role::Orchestrator | Role::Manager)` across two crates. Seven
copies is seven places for the next class to be forgotten in six of them — and a
class forgotten in the reaper alone is a human's own pane closed under them
while they were reading it.

| Guardrail | The lead pane | Its children | Why |
| --- | --- | --- | --- |
| `max_agents` (live cap) | exempt | **counted** | The cap contains delegate fan-out. The children are ordinary workers and take the unchanged `true` branch, so the launcher's "Max live agents" is what bounds a lead's fan-out; what is exempt is the seat, not the helpers. |
| spawn-rate backstop | n/a | **applies** | A runaway loop is possible in a human-driven pane too. |
| idle-kill reaper | never | **applies** | A lead pane is silent exactly when its human is reading. |
| stall watchdog | never | **applies** | Same reason, and there is no orchestrator to notify anyway — a stall notice about a lead would be a false report with no recipient. |
| docked/minimized on open | never | as today | #260's own argument: the pane the human works through is never hidden. |
| review-driver release | never | as today | A lead group has no review driver at all. |
| autonomy budget | **no** | n/a | It gates the autonomous idle tick, which a lead has none of. The human is the loop. |
| `kill_agent` on the pane | **refused** | as today | A hole this slice would otherwise open: `kill_agent` is on the lead's own surface and `require_in_group` passes for its own id, so without the guard a lead could end the human's pane from inside it. |

`Role::Solo` is deliberately **outside** `is_fixture`, and it is the control that
keeps the predicate from meaning "did a human ask for this pane". A solo pane is
human-launched too; it is out because it is in no orchestration group, so none of
the guardrails above is ever evaluated against it.

## Containment

`Containment::None`, on the **orchestrator's** argument and not the solo pane's.
A lead pane is a full working pane the human drives: it edits their code and runs
their commands, and it opens helpers to do more of the same. A deny tier here
would take away the CLI they launched, which is not something a toggle that
*adds* a capability may do. What bounds the fan-out is the cap and the
spawn-rate backstop.

## Public-contract changes in this slice

1. **`Role` gains the wire string `"lead"`** — in `agents.json`'s `role`, audit
   rows, `list_agents` JSON and `PaneFacts.orch.role`. An older build reading a
   lead group's `agents.json` fails that row's parse. Same posture as
   `Delivery::Regrounding`'s (`manager.md`, "One cost, stated"): downgrade
   safety was never on offer for these files.
2. **`group_summary`'s `roles` object gains a `lead` key.** Additive. The
   per-class tally is an exhaustive `match`, so the compiler forced the arm; a
   `roles` object that silently omitted it would tell the lifecycle panel a lead
   group has zero agents while a pane is plainly running. `live_delegates` is
   unchanged — it is derived through `counts_against_max_agents`, not by summing
   the tallies.
3. **`templates/lead.md`** is new and is **not** golden-pinned in this slice.
   The `pre222` pin exists to make an accidental edit to bytes a shipped pane
   already reads fail loudly, and nothing delivers this file yet — slice A ships
   the class, slice B the launch path. `block.md` and `workflow.md` are unpinned
   on the same terms. It joins the pin in the slice that delivers it, when its
   per-CLI content is settled, rather than being blessed here and re-blessed
   there.

No new dependencies.

## Two `unreachable!` arms this slice leaves, and why they are safe

`kickoff_prompt` and its mechanics core panic on `Role::Lead`. They are
unreachable in fact and not merely by convention: `kickoff_prompt` is called
only from a spawn that has an app handle, and no shipped path spawns a lead —
`kind_from_str` has no `lead` vocabulary, so neither a workflow file nor
`spawn_agent` can produce one, and `orch_lead_prepare` does not exist yet. Slice
B's first test hits both, so it cannot ship without giving them arms.

The two arms that are NOT panics are the ones a public path already reaches, and
that asymmetry is the rule this slice applied rather than a judgement call per
site: `Guardrails::clamped` normalizes every block's effective model and
`write_instruction_files` renders every block in a roster, so `default_model` and
`role_template` run the moment a lead block exists in a roster — before any pane
is opened. `default_model` answers `""` on the opencode/pi argument (the human
picked the model in their own launcher); `role_template` answers the real file.

## What this slice does not ship

- **The launch path.** `orch_lead_prepare` / `orch_lead_bind`, the group mint,
  the per-CLI `mcp_args` (including Claude Code's `--disallowedTools Agent`), the
  kickoff delivery, and the "exactly one root per group" invariant that group
  creation will enforce — all slice B.
- **The per-CLI subagent-disable table with citations**, which belongs with the
  command lines that carry it (slice B) rather than being written here against a
  plan.
- **The UI.** The launcher toggle, the `LEAD` pane chip, children indented under
  their lead in the Agents tab, and the close-lead confirm — slice C.
- **`spawn_agent(cli:)`**, the model/CLI-mixing half of the feature's own
  motivation. Independent of A–C and recommended as its own issue.
- **Restore.** A persisted lead pane and its children across an app restart.
