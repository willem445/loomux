# The manager — the human's own pane, and the channel that reaches it

*#1161. This note covers slice **M2**: the structural no-injection guarantee, the
`mailbox.json` registry behind it, the two MCP tools, and `Role::Manager`'s
enumerated tool surface. The capability class itself landed in **M1** (#1169).
Lifecycle (M3), the elicitation prose (M4), the unread chip (M5) and the
`role_hint: liaison` migration plus the user docs (M6) extend this note rather
than replacing it — where a section names a later slice, that slice owns the
paragraph.*

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

## What M2 does not ship

Stated so no surface here reads as advertising a mechanism that does not exist
(the #1026 line):

- **No prose teaches the two tools yet.** `manager.md` and the orchestrator's
  `{{MANAGER_NOTE}}` fragment are M4's, and the plan sequences them after this
  slice precisely because the tool names have to exist first. Until then the
  tools are discoverable through their own descriptions — which is why those
  descriptions carry the turn-start discipline and the relay rules rather than
  waiting for the templates.
- **No unread chip.** M5.
- **No launch-time spawn, and no reaper/watchdog/`max_agents` exemption.** M3.
  Until it lands, a manager is opened by nothing, so every behaviour here is
  reachable only through a hand-registered agent — which is how the tests reach
  it.
- **No `role_hint: liaison` deprecation and no user-facing docs page.** M6.
