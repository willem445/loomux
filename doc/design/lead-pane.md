# The lead pane — a human-driven pane that opens orrerix panes

*#2519. **Slice A** is the capability class: `Role::Lead`, its enumerated MCP
surface, the root invariant behind a child's `report`, and the guardrail
exemptions. **Slice B** (this note as it stands) is the launch path that mints
a lead group, hands the launcher the pane's MCP flags, types its kickoff, ends
its helpers when it dies, and refuses to resume it. The launcher toggle, the
pane chip and the Agents-tab grouping are **slice C**. Each slice extends this
note rather than replacing it. What is not yet shipped is one list, at the end.*

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
   branch runs `kind_from_str` on the recorded role and errors on `None`.

   A recorded row that *does* carry a block id takes the other branch, and
   **that branch is not a second structural refusal — do not read it as one.**
   `kind` is `None` by construction on a bare resume, so the lead caller's
   effective-class check reads the recorded *block*: `Some(Role::Lead)` for the
   lead's own block, refused as `resolves to kind "lead"`; `Some(Role::Worker)`
   for a worker block, which is **permitted**; and `None` only when the recorded
   block id no longer resolves at all. An earlier draft of this item said
   "both routes refuse", which is false of the corrupt-data subcase and points
   a reader at the wrong invariant (rev-final N1 / rev-std round 2).

   **What carries the property there is the effective-class check, and slice A
   said otherwise.** Its argument was the vocabulary — "no block can have kind
   `Lead` while `kind_from_str` cannot name one" — which was true while nothing
   minted a lead block, and `lead_prepare` is what makes it false: a real group
   on disk now holds one. Two of the three refusals above are unaffected (both
   read `kind_from_str` directly); this one is not, and it is the check in
   `mcp::call_tool` that refuses a lead its own block by the class the block
   resolves to. `a_lead_cannot_open_a_lead_by_naming_its_own_block` asserts the
   premise — a minted group really does hold a `Lead`-kind block — before it
   asserts the refusal, so the test cannot go vacuous the way the sentence did.

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

## Public-contract changes

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
3. **`templates/lead.md`** is new in slice A and **golden-pinned since slice
   B**. The `pre222` pin exists to make an accidental edit to bytes a shipped
   pane already reads fail loudly, and slice A delivered nothing — so it was
   blessed once, in the slice that ships the launch path, rather than blessed
   in A and re-blessed in B. It sits in `GOLDENS` and `LIVE` but not in
   `PRE222`, on `manager.md`'s terms: a default group declares no lead block,
   so no `lead.md` is written into its dir. Its `LIVE` key list is empty, which
   is a statement — the file carries no workflow-conditional prose at all.
   `block.md` and `workflow.md` stay unpinned.
4. **Two Tauri commands, `orch_lead_prepare` and `orch_lead_bind`** (slice B),
   in the `orch-control` ACL set. `orch_solo_prepare`/`SoloPrepared` are
   deliberately NOT widened to carry a lead: a lead is a different thing, in a
   real group rather than `__solo__`.
5. **`ExitInitiator` gains `LeadExit`** (slice B). Additive, and its own
   variant rather than a reuse because the three that existed all describe
   somebody in this process choosing to end a pane, while this one describes a
   pane ending because the pane its notice would go to has died.

No new dependencies.

## The two `unreachable!` arms, closed

`kickoff_prompt`'s body and `mechanics_core` panicked on `Role::Lead` through
slice A, on the argument that no shipped path reached them. Slice B's first
test is what closed that, and the reason it could not wait is CLAUDE.md
constraint 10: an unwind out of a synchronous `#[tauri::command]` is a process
**abort**, not a degrade — there is no `catch_unwind` anywhere on the WebView2
COM path — so "unreachable today" is not a safe place to leave a panic.

The residual slice A stated was "no producer, not no consumer", and it was the
right shape: `Role`'s serde form already carried the wire string `"lead"`, so
an `agents.json` row reading `"role": "lead"` PARSED. Slice B is the producer.
`mechanics_core` now returns a real non-overridable contract — reachable from
`render_block_instructions`'s replace-mode arm and from `copilot_agent_body`,
both of which run for every block in a roster — and `kickoff_body` returns the
lead's own first line, on `Role::Manager`'s pattern: no assigned task (the
human's first message is the task) and no "call `report(\"progress\")` and wait
for prompts", which names a tool a lead does not hold and a channel it does not
take. `a_lead_role_never_reaches_a_panicking_arm` pins both.

**`mechanics_core`'s arm is in lockstep with `templates/lead.md`**, the same
rule `Role::Manager`'s arm carries and for the same reason: a `mode: replace`
persona on a lead block reads that arm and nothing else, so a rule living only
in the template is a rule such a lead was never told. What the arm deliberately
drops is the template's "prefer these over your CLI's own subagents"
argument — persuasion rather than mechanics — and the per-tool descriptions the
MCP listing already carries on every turn.

## The launch path (slice B)

A lead pane is opened by the **human's own launcher**, not by orrerix, and that
one fact shapes everything below. orrerix never builds this pane's command line;
it appends flags to the line the human is about to run. So the launch is two
calls with the pty spawn between them, exactly as a solo pane's is:

1. **`orch_lead_prepare(cli, cwd, name, max_agents, auto_ops, idle_kill_minutes,
   max_spawns_per_hour, watchdog_stall_minutes)`** — mints the group and the
   lead's identity and returns `{group_id, agent_id, mcp_args}`. It runs *before*
   the CLI boots, because you cannot inject an MCP server into an already-running
   process.
2. the launcher spawns the pty with `mcp_args` appended;
3. **`orch_lead_bind(agent_id, pty_id)`** — records the pty and types the lead's
   `FreshKickoff`.

A kickoff that cannot be delivered does **not** fail step 3. By then the pane is
open and the human is looking at it, so an `Err` would report a launch failure
for a launch that plainly happened, and there is nothing for the caller to retry
or undo — `open_manager_pane_at_launch`'s argument, and its review-N3 correction
with it: the delivery OUTCOME is audited rather than discarded, because a lead
that never learned it is one is a pane whose behaviour nobody can explain from
the outside.

The guardrail arguments are the launcher's own fields, passed through rather than
defaulted: a group whose `idle_kill_minutes` arrived as `0` would have the reaper
switched off for the lead's helpers, which is the opposite of the guardrails
table above.

### What the mint creates, and what it refuses to

`OrchRegistry::lead_prepare` goes through `create_group_ex` — the same function
every launch uses — under the same `creation` mutex, so two toggles racing on one
repo cannot pick the same id. What it does **not** call is
`register_orchestrator_pane`: no `Role::Orchestrator` agent is ever inserted, and
the lead is the group's only pane.

The roster is `workflow::default_roster` over `Lead | Worker | Reviewer |
Planner` with `advanced_orchestrator: false` — which is the *Consent* section in
code. With that flag off, `create_group_ex` never opens the repo's workflow file
at all, so there is no roster to preview and no consent moment to skip.

Three things about that roster are deliberate and each has been mistaken for an
oversight at least once:

- **`Guardrails::clamped` prepends an orchestrator BLOCK**, because it prepends
  one to any roster declaring none. That is a row, not a pane. Slice A flagged it
  as a tripwire for this function, and the answer is that nothing in the mint
  turns a block into an agent — pinned by
  `lead_prepare_mints_a_group_with_the_builtin_roster_and_no_orchestrator`, which
  asserts the block IS there and that no agent holds it.
- **Reviewer and planner blocks are declared even though a lead may never open
  one.** The refusal a lead gets for `kind: "reviewer"` must be the caller-class
  check — the one this note argues for. Drop the blocks and the same call fails
  on "no such block": a different refusal, from a different mechanism, that would
  silently start passing if the class check were ever removed.
- **The lead's own block is named `lead`**, so its instructions file is
  `lead.md` — which `write_instruction_files` renders from `LEAD_TPL` for every
  block in the roster, before any pane exists. That file is what the kickoff
  points at.

**The one-root invariant is checked at the mint**, under the same lock, and the
check is a **backstop**: `next_group_id` picks the first candidate id with no
*live* agent, so a second toggle on a repo whose lead is running opens a second
group rather than a second root in one, and no test can drive the explicit
refusal through a public path. It is kept because the failure it guards is
silent — `deliver_relayed_to_root` is a `find` over a `HashMap`, so a group with
two roots would deliver a child's report to whichever came back — and because a
future caller that *names* an id (the `expect_group` shape
`create_orchestration_group` already has) would not be protected by liveness at
all. `a_group_never_ends_up_with_two_roots` pins the property over both groups
and says which mechanism delivers it.

### Telling the harness not to use its own subagents

The toggle's promise is that `spawn_agent` replaces the CLI's in-process
subagents. A lead that can still reach them will, because they are one call away
and `spawn_agent` is three — so where the vendor documents a way to take them
off the command line, orrerix takes it. `lead_mcp_args` is a solo pane's MCP
flags plus that denial, and the per-CLI answer is not the same shape three times:

| CLI | Native subagents | What the lead's command line does | Source |
| --- | --- | --- | --- |
| claude | the `Agent` tool (renamed from `Task` in 2.1.63) | `--disallowedTools Agent` | two documented facts composed — see below |
| copilot | yes, the `agent` tool (aliases `custom-agent`, `Task`) | nothing; instruction-only | the tool is a custom-agent `tools:` key, and the CLI's deny flags take three value shapes, none of them a bare tool name |
| pi | none | nothing to deny | `docs/usage.md`: pi "intentionally does not include built-in MCP, sub-agents, …" |
| opencode, codex | yes | **refused at `lead_prepare`** | no argv MCP seam — see below |

**Claude's flag is a composition, and saying so matters** because neither page
states it alone. The sub-agents page names the tool and the deny: *"To prevent
Claude from delegating to any subagent, deny the `Agent` tool itself with
`permissions.deny`."* The CLI reference gives the flag that spells a deny rule on
a command line: `--disallowedTools` is *"Deny rules. A bare tool name removes the
matching tools from Claude's context."* A bare `Agent` on that flag is therefore
the argv spelling of the deny the sub-agents page prescribes.
`CLAUDE_CODE_DISABLE_EXPLORE_PLAN_AGENTS` is narrower — the built-in explore and
plan agents only — and is not used.

**Copilot's row is a correction to the plan this slice was built from**, which
recorded Copilot as documenting no subagent tool name at all. Re-fetching says
otherwise: `custom-agents-configuration` names the `agent` tool and describes it
as *"Allows a different custom agent to be invoked to accomplish a task."* But it
names it as a key in a custom agent's own `tools:` list — a *file* orrerix would
have to hand the pane with `--agent`, replacing whatever agent the human chose —
and the flag seam this function writes into cannot express it: the CLI
configuration guide says *"To use the --deny-tool and --allow-tool options, you
must specify what type of tool you want to allow or deny"* and enumerates exactly
three, shell commands, `write`, and MCP server tools (the same list
`KNOWN_COPILOT_DENY_CATEGORIES` is pinned against). A bare tool name is not among
them. So a copilot lead is instruction-only, `templates/lead.md` asks it to
prefer `spawn_agent`, and nothing structurally stops it doing otherwise.
Inventing a `--deny-tool agent` value the vendor does not document would be a
claim, not a denial. The follow-up is the generated-custom-agent route, which is
a product decision about overriding the human's own `--agent` choice rather than
a wiring gap.

**opencode and codex are refused rather than degraded**, and that is where a lead
differs from a solo pane. `solo_prepare` gives a CLI with no argv MCP seam a
delivery-only identity, which is still a useful pane. A lead with no MCP server
holds *none* of the tools the toggle grants, so a pane that launched anyway would
be a lead in name only. The refusal names the follow-up — widening the prepare
seam to return environment pairs, which is what an opencode lead needs
(`OPENCODE_CONFIG_CONTENT` carrying the MCP config and `permission.task: "deny"`)
— so it reads as a missing seam rather than a policy.

### Lifecycle: the lead's death ends the group

Closing a lead pane ends its group, and a lead whose CLI simply *dies* must cost
the same. Otherwise its helpers keep running with nothing to report to: their
`report` would resolve no root, their panes would sit under a tab whose lead is
gone, and they would keep spending the human's tokens unattended.

`on_pty_exit` gets a `Role::Lead` arm that calls `end_lead_children`, which kills
each live delegate's pty and marks it dead directly — `end_group`'s technique,
for `end_group`'s reason: it skips the orchestrator-notification path, and there
is no orchestrator here to tell. It is deliberately **not** `end_group` itself,
which audits as actor `human` and performs a whole orderly teardown (worktree
cleanup when asked, the generated-agent-file reclaim, the pause marker). That is
the right thing for a human's End-group click and an overstatement for a pane
that crashed; what must happen either way is that no helper outlives its lead.

The arm is its own rather than a statement inside the existing delegate branch,
because that branch sends an exit notice and here there is no recipient — the
pane it would be typed into is the one that died. For the same reason each child
is stamped **`ExitInitiator::LeadExit`**, a new variant whose whole content is
that its notice has nowhere to go: `exit_notice_route` sends it to the audit log.

`a_dead_lead_takes_its_children_with_it` drives this through `on_pty_exit` — the
entry point every ending funnels through — and carries its own control: a dead
*worker* takes nothing with it, which a build that tore the group down on any
exit would fail.

### Restore: a lead group cannot be resumed

`resume_recorded_session` refuses every session belonging to a lead group, above
both of its branches, because the refusal is true of both. The lead's own pane is
a human-launched CLI orrerix never opened and cannot relaunch; its helpers,
rejoined into a group whose root is gone, would have no pane to report into at
all. The error is tagged `resume-lead-group:` like every other resume failure,
classified in `src/resumeerror.ts`, and deliberately not `start fresh`-able — a
fresh session would join the same rootless group. The way back is a new lead
pane.

**It is decided by a marker file, not by the roster, and that is worth reading
twice.** `group.json` does carry the lead block, but `read_blocks` resolves every
persisted `kind` through `workflow::kind_from_str`, which has no `lead` arm by
design — that absence is what stops a repo file declaring one and stops a lead
opening a lead. So an unknown kind is *dropped* on reload, and a roster read back
off disk would answer "not a lead group" for every lead group there has ever
been. `lead_prepare` writes a `lead` marker into the group dir and
`is_lead_group` asks that.
`a_reloaded_lead_group_loses_its_lead_block_but_keeps_its_marker` pins both
halves, so the day `kind_from_str` grows a `lead` arm that test goes red rather
than the marker quietly becoming redundant.

**The marker has a LIFETIME, and both ends of it are code.** This is the
`paused` marker's precedent taken whole rather than half, and taking half of it
was a real defect (rev-final B1). A group id is repo-derived and handed out
again — `next_group_id` returns the first candidate with no LIVE agent, and the
group directory is never removed — so a lead group whose pane has died leaves
its id, and a write-only marker, free for an ordinary orchestration to reattach
to. That group would then answer `is_lead_group()` for the rest of its life,
and every session in it would be refused as unresumable, citing a toggle nobody
flipped, with no start-fresh affordance to escape through (`lead-group` is
deliberately not one of the two kinds that offer it).

So the marker is **cleared in `create_group_ex`**, which is the one place a
group id is claimed, and re-written by `lead_prepare` after its own mint. Not
at teardown, and that is the load-bearing difference from `end_group`'s `paused`
remove: a lead that simply dies never runs `end_group` at all — its helpers go
through `end_lead_children`, which is not the same path. Clearing at the claim
covers the death, the crash and the deliberate close identically, and it covers
`Launch::Promote` too, which reattaches a dormant group by exactly the same id
selection. `an_ordinary_group_reusing_a_dead_leads_id_is_not_a_lead_group` is
the pin, and it is written so the two operands COLLIDE — same registry, same
repo, same id — because the first version of the resume test built its control
in a second registry on a second repo and could not have failed on this.

The write also moved BELOW every step of the prepare that can fail. It used to
sit above `write_mcp_config` and the empty-flags refusal, so either error return
left a `lead` marker on a group that never became a lead group — the same false
refusal, reached from the other side.

### A correction slice A's note could not make

Slice A's *Consent* section argued that a lead cannot open a lead partly from the
vocabulary: *"no block can have kind `Lead` while `kind_from_str` cannot name
one"*. That was true while nothing minted a lead block. **`lead_prepare` is what
makes it false** — a real group on disk now holds one — so the property moves
onto the **effective-class check** in `mcp::call_tool`, which reads the resolved
block rather than the `kind` argument and refuses with *"resolves to kind
`lead`"*. `a_lead_cannot_open_a_lead_by_naming_its_own_block` asserts the premise
(a minted group really does hold a `Lead`-kind block) before it asserts the
refusal, so the test cannot go vacuous the way the sentence did.

Nothing else in that section moves: a workflow file still cannot declare
`kind: lead`, and `spawn_agent(kind: "lead")` is still refused as an unknown kind
by the parse rather than by an arm.

## What is not shipped yet

- **The UI.** Everything a human touches: slice C. Until it lands there is no
  way to reach `orch_lead_prepare` at all — the two commands exist and nothing
  calls them.
- **`spawn_agent(cli:)`**, the model/CLI-mixing half of the feature's own
  motivation. Independent of A–C and recommended as its own issue.
- **Restore.** A persisted lead pane and its children across an app restart.
  Slice B ships the REFUSAL — `resume_recorded_session` turns away every
  session in a lead group, with the message a human sees — and not the
  capability. Bringing a lead back means re-minting the group, which is a
  launcher gesture and therefore slice C at the earliest.
