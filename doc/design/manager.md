# The manager — the human's own pane, and the channel that reaches it

*#1161. This note covers three slices. **M2**: the structural no-injection
guarantee, the `mailbox.json` registry behind it, the two MCP tools, and
`Role::Manager`'s enumerated tool surface. **M3**, the lifecycle: who opens the
pane, which guardrails skip it, and how both bare-resume routes into it are
closed. **M4**: the prose that teaches both ends to use them — the manager's own
contract, the `mode: replace` mechanics core it stays in lockstep with, and the
orchestrator's `{{MANAGER_NOTE}}` fragment. The capability class itself landed in
**M1** (#1169). The unread chip (M5) and the `role_hint: liaison` migration plus
the workflow-author docs (M6) extend this note rather than replacing it — where a
section names a later slice, that slice owns the paragraph.*

Companion notes: [liaison.md](liaison.md), whose stated promotion trip-wire is
why this class exists; [human-questions.md](human-questions.md) and
[needs-you-items.md](needs-you-items.md), the two registries the manager
presents but does not own; [workflows.md](workflows.md) for the block model.

## The requirement, and the one thing that makes it hard

A manager pane is the human's own conversation. They talk to it about the
project, it sharpens a rough feature request into something specific enough to
build, and it relays. The human's directive on #1161 draws one hard line
through the middle of that:

> I want a native channel for the manager and orchestrator to communicate for
> status updates, etc. **but I want to avoid prompt automation in the human
> manager pane.**

Everything else about the feature is additive and ordinary. That sentence is
not, because **every** pane-bound mechanism loomux has is push: a producer
calls `deliver_prompt`, loomux watches the pane until the CLI looks idle, and
pastes the text into it. Cross-workspace channels ride it. Watchdog notices,
answer notices, lock grants, watch results, the compact nudge — all of it is
the same door.

So the requirement is not "add a channel". It is: **make one pane unreachable
by that door, and then give its traffic somewhere else to go.** Those two
halves are this slice, and neither works alone — a pane nothing can reach is a
pane nothing can tell anything.

The requirement is deliberately **one-directional**. The manager pane takes no
injected text; the orchestrator pane takes delivery exactly as it always has.
Manager-to-orchestrator is still `message_orchestrator`, unchanged.

## The guarantee

**`deliver_prompt` refuses a `Role::Manager` target unless the delivery is one
`Delivery::permitted_into_manager_pane` names.** That is the whole of it.

Three properties are worth stating explicitly, because each is a decision
someone could reasonably have made the other way.

**It is at the chokepoint, not at the producers.** Every mechanism that puts
text in a pane calls `deliver_prompt` — there is no second path — so one
refusal covers `channel_send`, `send_prompt`, the watchdog and stall notices,
`[loomux] answer to q-N`, lock and watch notices, the compact nudge, and every
producer a future slice writes without having read this note. That last clause
is the point: N conventions at N call sites is a rule that holds until someone
adds the N+1th.

**It fires before admission.** The refusal sits above the dead-agent and
no-terminal checks and above every queue write, so a manager-targeted payload
never enters a pane's durable queue. That matters because `flush_paused_queues`
replays persisted entries *without* passing back through `deliver_prompt` — it
would be a second, unguarded door if anything could be sitting behind it.
Nothing can.

**The refusal is audited**, as `delivery-dropped` with
`RefusalReason::ManagerPane` (`manager-pane`), so it surfaces through the
shipped `front_door_refusals` machinery like any other declined delivery. It is
the first *policy* reason on that enum — the other five say loomux could not
deliver, this one says it will not, and no amount of resuming the pane, draining
its queue or binding it a terminal changes the answer. `RefusalReason`'s
`consequence` string says so to whoever reads the row.

### The two carve-outs, and why they are one rule

| `Delivery` | into a manager pane | why |
| --- | --- | --- |
| `FreshKickoff` | **permitted** | delivered before the pane is a conversation; it is how every agent learns what it is |
| `ResumeKickoff` | **permitted** | the same, on a resumed session |
| `Regrounding` | **permitted** (decision D2) | without it the directive-ledger survival mechanism is dead for this pane |
| `MidSession` | **refused** | everything else in the codebase |

`Regrounding` is a new `Delivery` variant carved out of `MidSession`, and the
choice of shape is the substance here. The obvious alternative was to leave the
post-compact notice as `MidSession` and give it a private back door past the
refusal — one extra function, no enum change, no persisted wire form to widen.

It was rejected on the rule CLAUDE.md states for guards: **a guard reads every
one of its inputs by one rule.** A carve-out spelled as a `Delivery` variant for
the kickoff and as a bypassing call for the re-grounding is two rules, and the
second is invisible to any test that enumerates the first — so "what may enter a
manager pane" would have no single answer to assert. As a variant, the permitted
set is a property of one enum, and
`exactly_three_delivery_kinds_may_enter_a_manager_pane` asserts it **as a set**,
not as three separate `assert!`s: a fourth carve-out folded in later fails the
count, which is the failure a per-variant test does not produce. That is the
counterfactual discipline CLAUDE.md asks for — a documented escape hatch is
pinned by a test that performs the edit, not by a comment saying it exists.

`Delivery::ALL`'s completeness is itself compile-forced: `Delivery::all_index`
is an exhaustive match, so a fifth variant does not compile until it is listed,
and the only index left over is past the end of a four-element array.

**`Regrounding` is otherwise identical to `MidSession`.** It answers every
existing lifecycle predicate — `wait_ready`, `confirms_autopilot_dialog`,
`recovers_lost_kickoff` — exactly as `MidSession` does, and a test pins that, so
it cannot quietly acquire a boot treatment by being added to one of those
`matches!` lists. Nothing about how any other class is re-grounded moved.

One cost, stated: `Delivery` is persisted (`queue::QueuedDelivery::delivery_kind`),
so a `"regrounding"` entry held through a pause and read by an *older* build
fails that entry's parse. Downgrade safety was never on offer for that file, and
the alternative was the asymmetry above.

### What the guarantee does not rest on

`connect_agents` refuses a manager on either side, and `send_prompt` names it
and points at `message_manager`. **Neither is load-bearing.** `deliver_prompt`
would refuse both anyway; these exist so the refusal is *legible* — a human
connecting two panes learns at the gesture instead of watching every later
message vanish into an audit line, and an orchestrator that reached for
`send_prompt` gets a redirect instead of a dead end. If either is ever relaxed,
the property still holds.

## `mailbox.json` — the durable pull registry

The mailbox is what makes the refusal survivable. It lives in the group dir
beside `questions.json` and `tasks.json`, and it is modelled on `humanq` down to
the id minting, the retention idiom and the refuse-rather-than-truncate posture.
The pure half is `crates/loomux-engine/src/mailbox.rs`; the registry methods that
hold the lock, write the file and emit the event are `OrchRegistry`'s, exactly as
`humanq`'s are.

### Why a registry rather than hardened channels

#271 W1 considered a pull-based `channel_read()` and rejected it: *the pane
transcript already is the inbox*. **For this one pane that principle inverts** —
the transcript is the human's conversation, so nothing may be typed into it,
which is precisely the case the old rejection never contemplated. Hardening
channels into something durable, workflow-declared and pull-consumed would
rewrite every property they have (in-memory, human-gestured, one-per-pane,
push-delivered); it would be a different feature wearing the same name.

### The row

```
{ id: "m-3", from: "orchestrator", kind: "update" | "question" | "reply",
  text: "…", created_ms: 1755…, read_ms?: 1755… }
```

- **`id`** — `m-N`, minted from the file's own high-water mark like `t-N` and
  `q-N`. Legible rather than opaque, never a capability, and monotonic rather
  than random, which also keeps the module clear of `getrandom` (constraint 2).
- **`from` and `kind` are loomux-built.** `from` is the caller's own id as the
  MCP server resolved it from its token; `kind` is a closed-set parse that
  *errors* on anything unrecognized rather than defaulting. There is no spelling
  of "post as someone else", so there is nothing to validate and nothing to
  forge — `humanq::AnswerSource`'s argument on a cheaper surface.
- **`text` is the one authored field**, sanitized on the way IN through
  `notify::sanitize_pane_text` with `Lines::Keep`, so a stored row can never
  carry a `[loomux]` span (brackets map to parentheses) or a control character.
  `Lines::Keep` rather than `Collapse` for `relay_payload_keeping_lines`'s
  reason: a status update is prose with structure, and reflowing it into one
  paragraph would be a legibility regression smuggled in by a hardening pass.
  Sanitizing at the single writer rather than at every reader is what keeps the
  rule from drifting.
- **`read_ms`** — absent is the definition of unread, and unread is what the cap
  bounds and what `check_mail` returns.

### The caps, and which side each protects

| | value | what it does |
| --- | --- | --- |
| `MESSAGE_TEXT_MAX` | 2000 | a longer post is **refused**, never cut |
| `UNREAD_MAX` | 32 | at the cap the **writer** is refused; no unread row is ever dropped |
| `READ_RETAINED` | 20 | read rows are pruned oldest-posted-first on the next write |

The unread cap is the one with an argument. Evicting the oldest unread row to
make room would discard status the human has not read in order to preserve the
orchestrator's ability to keep writing into a mailbox nobody is reading — which
is exactly backwards. A loud refusal reaches an agent that can do something
about it; a silent drop reaches nobody. The refusal names the real remedies:
`ask_human` and `request_attention`, which are durable and reach the human
wherever they are.

`MESSAGE_TEXT_MAX` matches `humanq::QUESTION_TEXT_MAX` deliberately, so the two
registries a reader compares have one number between them. The bound is not
about pane width — nothing here is ever pasted into a pane — it is about
context: `check_mail` returns every unread row in full, so cap × `UNREAD_MAX` is
what an orchestrator can make the manager read before the human has said a word.

### Reading is consuming, and the escape hatch that makes that safe

`check_mail` returns the unread rows, stamps them read, and reports how many
retained-read rows it left off. The projection and the stamp happen on the same
vector under one guard, so what is returned is exactly what was marked read — two
separate calls would let a post landing between them be stamped read without
ever being returned, which is a message silently consumed by nobody.

`include_read: true` returns the retained rows too and stamps nothing. It exists
because the consuming read marks mail read *before* the manager has said
anything to the human about it, so a session that dies or compacts in between has
consumed the human's status stream on their behalf. The re-read is the recovery,
and it is `list_tasks(include_all)`'s idiom rather than a new one. It
deliberately cannot **un**-read anything: a tool that can reset the record of
what was consumed is a tool that can replay the same status forever.

### Failure posture

`OrchRegistry::mailbox` is **loud** on a malformed or unreadable file, unlike
`tasks` and like `questions`: every mutation is a read-modify-write of the whole
file, so a read that answered "no mail" for a file it merely failed to parse
would let the very next `message_manager` overwrite it. `mailbox_unread` — the
chrome read behind the chip — is the one deliberate exception and answers 0,
because a badge has no error channel and nothing writes through that path.

`mailbox_lock` is a leaf: nothing holds a registry lock across it, and
`post_to_manager` resolves the manager block *before* taking it so the groups
mutex is never nested under it. There is no delivery on any path here, which is
the point of the feature — a mailbox write is what happens *instead* of typing
into the pane.

## The tool surface

### The two new tools

| tool | caller | listed when |
| --- | --- | --- |
| `message_manager(text, kind?)` | orchestrator | the group's roster declares a manager |
| `check_mail(include_read?)` | manager | always, on the manager's surface |

Write rights are asymmetric, and the asymmetry is the direction: the
orchestrator writes and cannot read, the manager reads and cannot write.
`questions.json`'s ask/answer split is the precedent — a channel whose two ends
can both do both is not a channel with a direction, it is a shared file. Both
are gated twice (#243): the listing is cosmetic, the `call_tool` check is the
gate, and `post_to_manager` refuses a manager-less group a third time next to the
write.

`message_manager` is listed only where a manager is declared, on the `locks`
precedent: naming a tool that writes to a pane which does not exist is a tool an
orchestrator will try, and every group that never declared a manager — nearly all
of them — pays no context for a feature it did not ask for.

Audit: `mail-post`, `mail-read`, and `mail-reject` for both refusals
(`no-manager-block`, `unread-cap`).

### `Role::Manager`'s whole surface, enumerated

It is a **positive enumeration** on `Role::Solo`'s pattern, not a subtraction
from a tier. Structurally it is a filter over the shared tier plus a short
extension list, which avoids a second copy of `list_tasks`'s description
drifting from the first — but it is **default-deny**: a tool added to the shared
tier by a later slice does not reach a manager unless someone puts its name in
the list and argues for it.

That direction is the fix for a real gap. M1 shipped with `report` granted to
the manager, because the surface was whatever the `role == Orchestrator`
else-branch left over, and `report`'s own dispatch arm excludes only the
orchestrator. A class whose instructions say it has no `report` could have
dispatched one. Enumeration is what makes "it has exactly these" a checkable
claim rather than a description of a fall-through.

| granted | why |
| --- | --- |
| `list_agents`, `get_state`, `list_tasks`, `get_task`, `list_verdicts` | "how is it going" is answered from the record, never by spending an orchestrator turn |
| `list_questions`, `list_needs_you` | the human's NEEDS-YOU panel unions both registries, so a pane that presents what is waiting must see both halves of it — the shared tier's own stated reason |
| `message_orchestrator` | the only outbound channel, and the whole of the manager's authority |
| `check_mail` | the only inbound one |
| `ask_human` | the liaison's shipped semantics unchanged: a durable decision row, and the answer notice goes to the **orchestrator's** pane, because un-blocking the work is what an answer is for |
| `request_attention` | argued below |
| `group_usage` | the human asks what this is costing in the pane they ask everything else in |
| `note_directive`, `request_compact` | self-scoped; the ledger matters more here than anywhere, since this pane's context *is* the record of what the human said |

| withheld | why |
| --- | --- |
| `report` | the manager is not a delegate with an outcome — its session never completes |
| `notify_when`, `list_notifications`, `cancel_notification` | a fired watch is a pane injection |
| `channel_send`, `channel_status`, and channel membership | channel delivery **is** injection |
| `withdraw_question`, `withdraw_attention` | both settle a row — any open row, not only your own |
| `spawn_agent`, `send_prompt`, `get_output`, `kill_agent` | fleet control; the orchestrator runs the fleet |
| `set_state`, `upsert_task`, `remove_task` | the board and the durable state are the orchestrator's record |
| `review_verdict`, the merge-queue tools | it reads no diffs and opens no gates |
| `session_digest`, the lock tools | scoped to other classes for their own reasons |

There is, as everywhere, **no answer tool at any tier**. `AnswerSource` gains no
variant: a manager answering a question put to the human would be an agent
claiming to speak as them, which is the exact theatre `humanq` exists to refuse.

### The `request_attention` decision

**Granted to the manager. `withdraw_attention` is not.**

This is not a widening M2 chose. `doc/design/liaison.md` records the trip-wire
firing and names its own answer in as many words: the raise was withheld from
the liaison *because* "the human-facing pane's raise belongs to `Role::Manager`
(#1161), whose own definition cites this trip-wire as the reason the fifth kind
exists at all — so the manager's enumerated tool surface, not a fourth row on the
table above, is where that grant goes." `mcp.rs`'s own `request_attention` arm
carries the same sentence. M2 is the slice that builds that surface, so it is the
slice where the promise is either kept or becomes a false claim on two shipped
surfaces.

Kept, and on its own merits rather than only on the citation:

- **The manager is the pane that most needs it and least has an alternative.**
  It takes no delivery, so nothing can poke it, and it acts only when its human
  speaks to it. When the human is away, a durable NEEDS-YOU row is the only
  surface it has — "the human is elsewhere" is otherwise a dead end for the one
  pane whose entire job is their attention.
- **It is the second root's own mechanism, not a convenience on top of it.**
  "A manager faces the human" and "a manager can put something in front of them"
  are close to the same sentence. The trip-wire was written against tools
  accumulating *around* a root on a class that was borrowing another's; here the
  class exists precisely to hold them.
- **The plan's tool table is silent on it, not opposed.** plan-867 predates
  #1151 slice B shipping `request_attention` at all, so its omission carries no
  argument — which is why this note makes one rather than citing the table.

`withdraw_attention` is withheld on `withdraw_question`'s precedent, which is the
same split one registry over: withdrawing **settles** a row, and it settles any
open row rather than only the one you raised. A manager whose raise is overtaken
names it to the orchestrator, exactly as the liaison does today.

`list_needs_you` is granted alongside, and has to be: a class that can raise an
item but cannot see the queue it raised into would be reasoning about a list the
human can see and it cannot.

## `orch_mailbox_status`

One new `#[tauri::command]`: `orch_mailbox_status(group_id) -> usize`, the
manager's unread count, parsed through `command_group` (constraint 6). It reads
0 for every group that declares no manager, and 0 on a read failure — chrome has
no error channel, and the registry's loud read is untouched. M5's unread chip is
its consumer; until then it is a read nothing calls, which is cheaper than a
second visit to the ACL surfaces later.

**Registering it is a five-place change**, and the count is worth recording
because two of the five are easy to miss and neither fails loudly at the place
you would look:

1. `src-tauri/src/lib.rs` — the `generate_handler!` list.
2. `src-tauri/src/command_manifest.rs` — `APP_COMMANDS`, **and** the
   `// orchestration (N)` count comment above the block.
3. `src-tauri/permissions/sets/orch-read.toml` — the `allow-…` grant. The
   aggregate `main-ui` set pulls this in; `capabilities/default.json` names only
   `main-ui` and needs no edit.
4. `src-tauri/permissions/autogenerated/orch_mailbox_status.toml` — generated by
   `build.rs` from `APP_COMMANDS`, and **committed**.
5. `src-tauri/tests/acl_manifest.rs` — the `stub_commands!` list, and the
   `app_commands_len_is_N` tripwire's name, count and changelog clause.

See [acl-manifest.md](acl-manifest.md) for what each of those diffs against.

## Lifecycle (M3) — who opens the pane, and what never touches it

### The launch shape

**loomux opens the manager pane at group launch, beside the orchestrator's own
pane.** `open_manager_pane_at_launch` runs inside `register_orchestrator_pane`,
off the roster's own declaration — not from anything the orchestrator does, and
not sequenced behind its bind. That is the whole of the first-class claim: the
human's interface exists because the repo's `.loomux/workflow.yml` declares it,
not because an orchestrator was asked to open one and complied.

What it guarantees is that the manager pane is *requested* at launch. It does not
guarantee an ordering against the orchestrator's kickoff: the two panes bind
independently, and making one wait on the other would either stall the
orchestrator behind a `BIND_TIMEOUT` or make the human's pane a dependant of the
one it exists to relay to.

A group whose roster declares no manager block — which is every default
(no-workflow) group — opens nothing and says nothing.

If the open fails, the launch still succeeds. A group without a manager pane is a
degraded group, not a broken one — direct human access to the orchestrator was
never removed.

**The failure arm has two cases, and they are not the same event.**

- **"One is already open."** The singleton refusal is genuinely reachable here:
  `resume_recorded_session` documents a double-restore race where two restores of
  one session can both pass its liveness pre-check (#799). This case is audited
  (`manager-already-live`) and **nothing is delivered** — the human has their
  pane, and the degrade notice below is precisely the text that triggers the
  orchestrator's "manager not live" fallback. Telling it the human is unreachable
  while they are typing into a live manager is worse than saying nothing. Decided
  by re-asking the registry (`has_live_manager`), never by matching refusal text.
- **"It could not open."** Audited as an `error`, and told to the orchestrator,
  because the orchestrator is the party that can act on it: its fallback is to
  take the human's input in its own pane, and it cannot take that branch on a
  fact it was never given. That notice's **delivery outcome is itself audited**
  rather than discarded: this arm runs on a background thread racing the
  orchestrator's own bind, so the notice can arrive before that pane has a
  terminal and be refused. The whole degradation story rests on the orchestrator
  having been told, so a dropped notice has to be findable in the trail rather
  than inferred from its absence.

The singleton rule itself is one expression (`is_live_manager_of`), shared by
`has_live_manager` and by `spawn_agent_bound`'s check under the already-held
agents guard — two hand-written copies of "same group, `Role::Manager`, not
dead" would be the same divergence `counts_against_max_agents` exists to prevent
for the cap.

**The pane opens expanded** (`spawn_opens_minimized` exempts `Role::Manager`,
M1), in the repo root with no worktree, with `Containment::NoEdits`.

### The resume path

When the launch itself resumes a conversation
(`SessionOrigin::resumes_session()` — a dormant group brought back, or a promoted
pane), the manager reopens **its own last recorded session**: the last-touched
`agents.json` row for the manager block that carries a session id, plus the name
tier a human rename earned (#95r). A launch that starts fresh, and a group with
no such row, cold-starts. The continuity matters more here than anywhere else in
the group: this pane's transcript *is* the record of what the human said.

**A resumed launch reads the PINNED roster, not the file.** `create_group_ex`
sets `reads_workflow_file = false` for `Launch::Resume`, so a resumed group runs
the roster persisted in its `group.json` and never re-reads
`.loomux/workflow.yml` (#255/#459 — the roster is the thing the human consented
to at launch). The consequence for this feature, stated because it is
user-visible and easy to read as a bug: **adding a manager block to a repo's
workflow file does not give a DORMANT group a manager when it is resumed.** It
gets one on its next fresh launch. The mirror case is the same rule and equally
deliberate — a manager removed from the file stays with a resumed group that was
launched with one.

**A resumed manager is typed nothing.** `spawn_agent_bound` delivers a follow-up
on a resume only when the spawn carries a task, and this one never does — so the
pane simply reopens with its history. `Delivery::ResumeKickoff` *is* in the
permitted set (the table above), and this path declines to use it: that carve-out
is for a pane that has not become a conversation yet, and a resumed manager pane
is nothing but one. The fresh arm's kickoff is the pane's first line, which is
the case the carve-out is actually for.

### Both bare-resume routes into `spawn_agent_ex`

M1 refused `spawn_agent(kind: "manager")` and `spawn_agent(block: "<a manager
block>")`. Both of those test the caller's **arguments**, and block inference in
`mcp::call_tool` runs after them — so a bare
`spawn_agent(resume_session: <a manager session>)`, naming neither, reached
`spawn_agent_ex` holding a manager block having passed every argument check.
There are two such routes, and M3 closes both:

| route | how the manager block is acquired |
| --- | --- |
| post-#222 roster row | the recorded `block` id is inherited verbatim (#254) |
| pre-#222 roster row (role only, no block id) | `kind_from_str("manager")` resolves the class, and `block_for` takes its default block |

On both, the `role` **argument** reaching `spawn_agent_ex` is `Role::Planner`
(`kind.unwrap_or(Role::Planner)`), and `block.kind` wins over it — so only a
check on the resolved block sees them at all.

The enforcement is one line in `spawn_agent_bound`, the twin of the guard the
orchestrator block has had since #222:

```rust
if block.kind == Role::Manager && named.is_some() { /* refuse */ }
```

`named` is the block id the caller supplied. It is `Some` on every
agent-reachable route, including both resume routes above, and `None` on exactly
the two openers loomux owns — `open_manager_pane_at_launch` and the session
browser's manager rejoin — which resolve the block **by class**
(`block_for(Role::Manager)`). For a manager the two resolutions name the same
block: `workflow::MANAGER_MAX` is 1, so "the first of that kind" is "the only
one". That is what lets the refusal stay unconditional on the *shape* instead of
being softened into a guess about who is calling.

`mcp::call_tool` keeps a third refusal, on the **effective** block after
inheritance, for the sentence rather than the enforcement (#243's double gate) —
an orchestrator that reached for a manager session wanted to reach the *human*,
and the answer to that is `ask_human`, or `message_manager` for status.

### One live manager per group

`MANAGER_MAX` bounds what a workflow file may **declare**, not how many panes one
declaration opens, and the two openers loomux owns can genuinely race — a human
clicking Resume on a dead manager session while a relaunch brings the group's own
one up. `spawn_agent_bound` refuses a second live `Role::Manager` in the same
group, checked under the agents lock beside the race-safe cap re-check. Two
manager panes would be two conversations the human has to notice are different,
and one mailbox drained by whichever read it first.

### The exemptions, keyed on `Role::Manager`

Three guardrails exist to contain **delegate fan-out** — the axis an orchestrator
controls. A manager is not a delegate it opens, so all three are keyed off the
class, never off a `role_hint`:

| guardrail | manager | why |
| --- | --- | --- |
| idle reaper (`idle_reap_candidates`) | exempt | a manager spawns with no task — the human's first message *is* the task — so `idle_since_ms` stamps at birth. Unguarded it is taken on the first sweep past `idle_kill_minutes`, before the human has typed anything, and the notice goes to the orchestrator's pane rather than to the human sitting in front of the one that vanished. An idle manager is a manager whose human is away: that is its normal state. |
| stall watchdog (`watchdog_tick`) | exempt | its silence means the human is reading. The notice would be a false report about a pane the human is looking at, delivered to one they are not. |
| `max_agents` (`counts_against_max_agents`) | exempt (decision **D3**) | the human's interface must not be competable with a worker slot — and on a cap of 1, counting it would leave a group with a manager unable to spawn any worker at all. The spawn-rate backstop rides the same predicate: it guards against a *runaway orchestrator*, and this pane is opened once per group by the launch path. |

The reaper's exemption is keyed on `a.role`, never on the `standing` hint set the
liaison uses: that set is a property of the group's **roster** (which block ids
are liaisons) and the class is a property of the **agent**, so a manager stays
exempt on a pane whose block id no longer resolves — a `workflow.yml` edited
mid-session, which #459 already treats as a live reality.

The watchdog names `Role::Manager` explicitly even though a manager opened by the
launch path arrives task-less and is therefore *already* skipped by the
idle-clock clause. Those are two different rules that agree today: the idle clause
says "nothing is assigned", which is a fact about which paths exist, while "loomux
never nags the orchestrator about the human's own pane" is the rule.

`counts_against_max_agents` is one pure predicate, and **every site that decides
this question calls it** — four functions, five decision points:
`live_delegate_count` (the value enforcement reads), the cap-refusal roster (the
names in the message), `spawn_agent_bound` twice (its fast-path check and its
race-safe re-check), and `group_summary`'s `live_delegates` (the number the
lifecycle panel shows). A `grep` for the call shows **seven**: the race-safe
re-check calls it three times — once to gate the block, then inside each of the
two filters that count the slot-holders and name them.

Four of those sites had independently spelled `role != Role::Orchestrator`.
`group_summary` had spelled the rule a *third* way again — a hand-sum of
per-class tallies (`worker + reviewer + planner`) — and was converted in the same
edit even though that sum already produced the right number. The reason is the
failure it would otherwise wait for: a sixth `Role` forces a new arm in
`group_summary`'s exhaustive `match` (the compiler sees to that) but is silently
missing from the sum, so the panel would under-report the number
`spawn_agent_bound` enforces, and the cap-below-live warning the panel exists to
give would go quiet at exactly the cap where spawns start failing. The predicate
defaults a new class to COUNTED, so routing through it makes that drift
impossible rather than merely unlikely.

A class therefore cannot be exempt from the count, named in the refusal message,
and omitted from the panel's total independently of each other.

`recommend_capacity` **inverts** M1's `+1`. `recommended` answers "what must the
cap be for every declared tier to be live at once", and a class the cap does not
apply to is live at any cap — the rule the field already stated for the
orchestrator. `extra_tiers` loses its manager row for the same reason: that list
is "what a cap below `recommended` can never keep live", and the answer for an
exempt class is nothing. `minimum` never moved and still does not: it is what one
review round costs, and a review round does not involve the manager.

### What M3 deliberately does not do

- **No `kill_agent` protection.** An orchestrator can still kill a manager pane.
  The plan routes "never kill the manager" to M4's `{{MANAGER_NOTE}}` prose, and
  the degradation position is unchanged: kill or close the pane and the group is
  byte-for-byte its pre-manager behaviour. Making it mechanically unkillable is a
  separate decision, and it is not this slice's to take.
- **No new mid-session delivery.** The permitted set is still exactly the three
  rows in the table above. M3 adds a producer of `FreshKickoff` into a manager
  pane (the launch open) and no producer of anything else — and the fresh
  kickoff is now reachable in production for the first time, where M2 could only
  reach it through a hand-registered agent.

### Why nothing reopens a dead manager (#1433)

M3 left two premortem items filed rather than absorbed: the launch-time manager
spawn can fail with the human told nothing, and nothing reopens a manager pane
that dies. Both reduce to one question — *the pane is not there; who says so, and
does anything put it back?*

**Nothing puts it back, and that is a decision, not a smaller scope.** The
user-facing page already promises, in `docs/features/manager.md`:

> Nothing is taken away. The orchestrator pane, the steering strip, the task
> board, the NEEDS-YOU panel and the questions you answer there all work exactly
> as they did — and if you close the manager pane, the group behaves as it always
> has. Talking to the orchestrator directly is never removed.

So closing that pane is a legitimate act the human is invited to perform. And
nothing in the registry can tell a deliberate close from a crash: both arrive as
the same pty exit and the same `Dead` row. An automatic reopen would therefore
have to guess, and half its guesses would reopen a pane its human had just shut —
contradicting a shipped promise on the strength of an inference. The rejected
alternative is worth stating plainly because it is the obvious one: a one-shot
respawn keyed on the manager going `Dead`. It fails on the same point regardless
of how the shot is bounded, and it adds a second failure mode of its own (a CLI
that cannot start respawning against a retry policy nobody asked for).

**What ships instead is a notice.** `group_summary` gains `manager_declared` —
whether the roster this group is RUNNING declares a manager block — beside the
existing `roles.manager`, which counts live ones. `roles.manager` alone cannot
answer the human's question, because it is `0` both for a group whose manager is
missing and for the overwhelming majority of groups, which declare none at all.
The pair is what carries the fact, and `src/group.ts`'s `managerAbsenceNotice`
turns it into the group panel's line: *manager declared · not open*, with a
tooltip that says why nothing is coming and names the route back — the session
browser, which is the human-side opener `spawn_agent_bound`'s manager refusal
already points at.

One surface covers both of #1433's items deliberately. From the panel they are
the same fact: the pane is not there. *Which* of the two happened — a refused
open at launch, or a death since — is exactly what the audit trail records
(`manager-opened`, `manager-already-live`, or an `error` carrying the refusal),
and putting it in a chrome line would be a second copy of the trail, free to
drift from it.

**`manager_declared` reads the resolved roster, never the repo's file.** A group
resumes on the roster it launched with and never re-reads
`.loomux/workflow.yml`, so the file is not what a running group is running —
the same rule that makes "adding a manager block does not give a dormant group
one on resume" true (see *The resume path* above).

**The failed-open arm is reachable in tests now.** `spawn_agent_bound` refuses a
block whose `cli` no build supports, and its own comment says where such a block
comes from: "an unsupported one here means a hand-edited group.json". A launch
with no workflow file falls back to the caller's roster, so a test can hand in
exactly that roster and reach the arm. The refusal lands on a string several
steps before any process would start, so constraint 3 (never run a real agent
CLI) holds by construction rather than by care — nothing is executed, and the
test asserts the group still launches, that no manager row exists, and that the
audit carries both the failure and its reason.

## M4 — the prose, and why it lands on three surfaces at once

M2 shipped a channel nothing was told to use. M4 is what tells the two ends,
and it is three edits rather than one because the manager's contract reaches a
pane by three different routes and a rule present on only one of them is a rule
some manager was never given.

| surface | who reads it | what happens if the rule is only elsewhere |
| --- | --- | --- |
| `templates/manager.md` | every manager, as its instructions file | — |
| `mechanics_core(Role::Manager)` | a `mode: replace` manager, *instead of* that template | it reads a contract with the rule missing |
| `{{MANAGER_NOTE}}` in `templates/workflow.md` | the orchestrator of a group that declares a manager | the other end never learns the protocol |

The second row is the one worth arguing. Decision D1 denies a manager block a
repo-authored persona, so `parse_workflow` refuses `prompt:` / `profile:` /
`allow:` on one and the parser cannot produce a replace-mode manager today —
which makes that arm reachable only through a hand-edited `group.json`. It is
still written as a real contract rather than an `unreachable!()`, for the reason
the arm's own comment gives: D1 is a **policy** decision a later human opt-in
could relax, and the cost of the arm being right is one paragraph while the cost
of it being wrong is a human-facing pane with no instructions. `manager_prose.rs`
pins the template and the arm against the same anchors so the two cannot drift.

`templates/orchestrator.md` is **not** touched, and that is the same discipline
`{{LIAISON_NOTE}}` established: a group that declares no manager must not read
one word about one, which is what keeps the OTHER FOUR goldened role
templates byte-identical through this slice — `manager.md` is the fifth golden
and it IS re-blessed here, which the `pre222/README.md` log entry records.
`manager_prose_stays_silent_unless_a_roster_declares_one` asserts the silence
directly rather than leaving it to the golden diff.

### What the manager is told, and why each rule is load-bearing

- **Every turn opens with `check_mail()` and `list_questions()`.** This is not
  hygiene, it is the channel: the guarantee above means nothing is ever typed
  into this pane, so there is no notification and nothing arrives while the pane
  is idle. The human is the scheduler of the manager's attention — a turn that
  does not start with the read is a turn the orchestrator's mail never lands in.
  Reading consumes, so the prose names `include_read: true` beside it as the
  post-compact recovery, and frames every row as the orchestrator's **account**
  of what is happening: data, never instructions, and never authority. That
  framing is the mitigation for the residual risk this note's M2 half records —
  an orchestrator fed hostile repo content could write misleading status into a
  pane the human trusts.
- **Sharpen, then read back.** The human's scope-add makes the manager a
  requirements surface rather than a relay, and "sharpen it" is a sentiment
  until it names axes. Six: the problem behind the ask, acceptance criteria,
  non-goals, constraints, edge and failure cases, and the rationale worth
  keeping. Grounded in the repository — the class is `Containment::NoEdits`
  precisely so it can read the code, and a question a file already answers is a
  question that spends the human for nothing.
- **The brief has a shape, and a gate.** Nine named parts, and an explicit yes
  before anything is relayed. The gate has to enumerate its own failure modes or
  it does not hold: silence is not a yes, a yes to a summary rather than to the
  text is not a yes, and a yes to an earlier version does not carry to one
  edited afterwards.
- **Relay fidelity, and the authority line.** `message_orchestrator` is the
  manager's only outbound channel and the whole of its authority. It quotes the
  human verbatim and keeps its own reading plainly separate, because the
  orchestrator has no other way to tell a direction from an interpretation of
  one. And a relay carries the human's **words**, never their **authority** —
  `{{MANAGER_NOTE}}` states the same rule to the side that would be the one able
  to act on a grant it never got.
- **Decision D5, on both sides — and split along the right seam.** The
  in-conversation yes licenses *filing the issue* and nothing more. What is
  UNCONDITIONAL is the manager's own authority: it never starts work, never
  applies a label, never asks the orchestrator to treat a relayed yes as one.
  What is MODE-DEPENDENT is the funnel — under the opt-in default and plain
  autonomous mode the start-work label is the sole start-work consent, while
  `orchestrator.md`'s invariant 8 inverts that default under **full autonomy**
  and demotes the labels to priority hints. A manager group can be in full
  autonomy (`full_autonomy_groups` is independent of `advanced_orchestrator`),
  so prose stating the funnel unconditionally is false there — and it is false
  in the direction that matters, telling the human their label is a gate the
  orchestrator is not in fact waiting on. Both prose surfaces state the
  manager's half flatly and the funnel conditionally; what stays flat in every
  mode is that full autonomy widens what may be STARTED and never what may be
  SHIPPED. Stated on both sides on purpose — one could otherwise honour it
  while the other did the thing.

### The mode caveat, what pins it, and what pins cannot reach

D5's seam (above) is the first place the manager's contract describes the
**orchestrator's** mode machinery, and that has three consequences worth stating
here rather than leaving in a review thread.

**1. The upgrade unit is now invariant 8 plus the manager's copies of it.** Three
manager surfaces and a golden assert what `orchestrator.md`'s invariant 8 says:
`templates/manager.md`, `mechanics_core(Role::Manager)`, `docs/features/manager.md`
and `tests/fixtures/pre222/manager.md`. If a later slice changes how modes work —
M3's lifecycle, or any rework of the label funnel — all four go stale together.
Whoever edits invariant 8 should grep the other four; nothing mechanical will point
at them.

**2. What the five pins do and do not cover.** `manager_prose.rs` pins each half of
the seam on both contract surfaces — the manager's unconditional authority, the
mode-dependent funnel, and the shipping line — so a reword that drops one half goes
red naming the half it dropped. What they **cannot** detect is invariant 8 changing
underneath them: they are presence pins on this side of the boundary only, and
`docs/features/manager.md` is not pinned at all, because no backend test reads
`docs/`. The alternative that would close it is to pin the RELATIONSHIP rather than
each side's words — read `ORCHESTRATOR_TPL`'s invariant 8 and assert the manager's
surfaces carry a caveat whenever it says the default inverts. That is the shape to
reach for if this drifts again; it was not built here because it is a guard with its
own design questions, not a line in a prose slice.

**3. The sweep M5 and M6 owe is "can the pane actually DO this", not "is this tool
granted".** Two guards already stop the prose naming a tool the manager cannot call
(`the_managers_contract_never_names_a_tool_it_does_not_have`, and its default-deny
sibling deriving the granted set from `mcp.rs`). Neither catches an **instruction**
to do something no tool supports, and this slice shipped exactly that and had it
caught in review: the first cut of the mode caveat told the manager to "find out
which mode this group is in", which nothing on its fourteen-tool surface reports —
the mode reaches the orchestrator by a delivery into ITS pane, the one thing that
never happens to this one. The prose now says it cannot look this up and to ask the
human, who set it. When adding prose here, read each imperative and ask which
granted tool performs it; a sentence with no answer is this defect.

### What the orchestrator is told

`{{MANAGER_NOTE}}` interpolates the declared block's own id, because every rule
in it addresses a specific pane. Its claims are scoped to what M1 and M2
actually shipped, deliberately (the #1026 line): `spawn_agent` refuses a manager
by `kind` and by `block`, so "you do not open it, and you cannot" is a fact
about code; `deliver_prompt` refuses the pane, so the fragment names
`message_manager` rather than letting the orchestrator discover a `send_prompt`
error. It does **not** assert the reaper, watchdog or `max_agents` exemptions,
although M3 (decision D3) has landed and the table above records all three as
`exempt`. That is deliberate and it is a *separation* argument, not a sequencing
one: the fragment is an operating-instructions surface — what the orchestrator
must DO — while the exemptions are enforced in code (`counts_against_max_agents`,
the reaper's role-keyed skip, the watchdog's `Role::Manager` arm) and documented
in the table above. An orchestrator acting on the note cannot violate them, and a
second surface asserting them would be a copy free to drift from the predicates —
with nothing pinning it, since no test reads the note for exemption claims. What
the fragment says instead is an *instruction*: never `kill_agent` the manager.
Inside the group the orchestrator is the one thing that can end the human's own
pane.

The brief hand-off is the fragment's other half. The manager's containment keeps
`gh`, so "the manager never files the issue" is instruction-backed rather than
structural — the *designed* path keeps issue authority where it already lives:
the orchestrator files it quoting the brief verbatim, applies the label its own
intake rules would apply, and posts the issue number back with
`message_manager(kind: "reply")`. The issue is the durable artifact; no brief
registry exists and none is planned.

### `block.md`'s recap is per-class now (w-875 N9)

The block note's closing paragraph recaps "the mechanics the workflow file did
not change", and it used to be one sentence for every class: the MCP tools, the
`report(status, summary)` discipline, the branch → PR flow, and the human gating
every merge. A manager whose block id is not the reserved `manager` — `- id:
mgr-desk`, which is the only manager `block_note` renders for, since a builtin
id with no persona early-returns — was being handed that as its contract. Every
clause of it is false for a pane with no `report`, no branch and no work of its
own to have merged, and it arrives in the one paragraph that tells the reader to
believe this file over its own instructions. So it reads as the correction, not
as a mismatch to resolve.

`{{MECHANICS_RECAP}}` replaces the list. Same three-clause shape either way, so
the sentence still lands as a recap rather than as a second contract; for a
manager the three become the MCP tools, the `check_mail()` its turn opens with,
the read-back before any relay, and the rule that it holds no authority the
human has not exercised themselves. It is never empty, so it sits mid-line like
`{{BLOCK_KIND}}` — the line-final placeholders in that template keep the
property that lets them render to nothing.

## What M2 and M3 do not ship

Stated so no surface here reads as advertising a mechanism that does not exist
(the #1026 line):

- **The prose that teaches the two tools is M4's, and it has landed** — see
  *M4 — the prose* above. The tool DESCRIPTIONS still carry the turn-start
  discipline and the relay rules in their own words, which was M2's answer to
  shipping a channel before the templates named it, and stays the floor for a
  manager whose instructions file it never read.
- **No unread chip.** M5.
- **No `role_hint: liaison` deprecation warning, and no page for the workflow
  AUTHOR.** M6. The user-facing page for the human who *talks* to the manager
  (`docs/features/manager.md`) ships in M4; what M6 still owes is the
  `author-loomux-workflow` skill row, the worked `kind: manager` example and
  the supersession note.

## What M4 does not ship

- **No unread chip.** M5 — until it lands, the human's only signal that mail is
  waiting is the manager telling them after it reads.
- **No `role_hint: liaison` deprecation warning.** M6.
- **No user-facing page for the workflow author.** `docs/features/manager.md`
  ships here and is written for the **human who talks to the manager**; the
  `author-loomux-workflow` skill table, the worked `kind: manager` example and
  the supersession note are M6's.
