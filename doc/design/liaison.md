# The liaison block (`role_hint: liaison`)

A liaison is the pane a **human** talks to. It converses in natural language,
reads the group's board and state to answer "how is it going", presents the
orchestrator's questions for the human, and relays the human's intent back. It
holds no orchestration authority of its own.

This note covers the `role_hint: liaison` value itself — the public
`.loomux/workflow.yml` surface it adds — the two capability rules keyed to it,
the prose that makes a declared liaison do anything (**The prose**), and how the
pane starts, is skipped and ends (**Lifecycle**). The user-facing documentation
is a separate slice and is **not** described here as though it shipped.

## Why `kind: reviewer`

`role_hint` must pair with the capability class it is meaningless without, so
the interesting question is which of the four classes a human-facing pane
belongs to. Each of the other three is wrong for a reason, not by elimination:

- **`planner`** auto-closes: `report("done", …)` reaps the pane
  (`close_completed_planner`). A liaison is a standing conversation; a pane
  that exits when it answers is a correspondent who hangs up mid-sentence. It
  is also channel-banned on both sides and forced unattended.
- **`worker`** carries write and git-mutation authority — edits, commits,
  pushes, PRs. A liaison is *defined* by holding none of that. Giving it the
  class and then relying on prose to stop it is the shape this codebase
  refuses everywhere else.
- **`orchestrator`** is the trust root and is not a repo-writable surface at
  all; a workflow file may pin its `cli:`/`model:` and nothing more.

`reviewer` is what remains once those are ruled out, and it is a positive fit
rather than a residue: `Containment::NoEdits` (the editing tools — Edit, Write,
NotebookEdit — denied at the CLI, while the shell and `git`/`gh` stay, so it can
read the audit log and the board without poking the orchestrator), persistent
(no auto-close), channel-eligible, and already able to
`report`/`message_orchestrator` — which is the whole downward wire. Nothing new
is invented for it: no dependency, no MCP tool, no persisted state.

**Be exact about the size of that tier, because the argument leans on it.**
A reviewer is contained but **not read-only** — `is_read_only()` is false for
it, matching only `Containment::ReadOnly` (the planner's tier), and
`doc/design/orchestration.md` states the same: *"a reviewer row is contained but
NOT read-only, and keeps its shell git."* `NoEdits` removes the frictionless,
default path to editing a file and leaves the shell path (`sed -i`, a heredoc,
`python -c`) reachable, because denying that would mean denying `Bash`. It is
containment of the accident, not of the adversary.

The residual that follows, stated rather than absorbed: a verdict is a file
under `verdict_dir(group, pr)`, and a shell can write a file. So the guarantee
this note describes — the liaison cannot record a verdict — is exactly true of
**the MCP path**, which is the whole reachable graph for an agent using its
tools. It is no weaker than any other reviewer's, since a plain reviewer could
write a sibling's verdict file the same way; it is simply not the stronger
"cannot write" property, and a later slice must not build on the belief that it
is.

A fifth first-class `Role::Liaison` was rejected for the same reason the
advisor's was (`supervisor-skills.md` §13): it would touch ~60 sites across
`mod.rs`, `workflow.rs`, `mcp.rs`, the TypeScript mirror, a new template file
and four golden fixtures, and buy no capability the reviewer class plus two
hint-keyed rules cannot already express. If the exception list below ever grows
past a couple of entries, *that* is the trigger to revisit — not aesthetics.

## Three hint-keyed capability rules, in two directions

`review_verdict` is **withheld** from the liaison; `group_usage` and `ask_human`
— both otherwise orchestrator-only — are **offered** to it. All three are argued
separately below because they are separate arguments: closing a fail-open window
and widening a capability answer to different bars, and the two widenings answer
to different bars again (one is a read, one is a write), so a note that presented
them as one list would let each borrow the previous one's obviousness.

### `review_verdict` is withheld

A reviewer may record a verdict, and a verdict is not a notification: it is
durable, attributed state that this repo's `gh` interceptor reads before
allowing `gh pr merge`. A liaison rides the reviewer class for its *posture*
and reviews nothing. A pane that never reads a diff must not be able to record
the PASS that opens a merge gate — so the liaison is denied it.

Denied at **all three layers a verdict passes through**, which is the same
discipline the class check itself gets:

1. `mcp::tool_defs` — the tool is not listed for a liaison-hinted reviewer.
   Cosmetic, like every listing here.
2. `mcp::call_tool`'s `review_verdict` arm — the dispatch check, refusing with
   a reason that names the rule.
3. `OrchRegistry::record_verdict` — next to the write, reading the hint from
   the group's own roster rather than from anything the caller carried in, so
   this layer is not a second copy of the answer layer 2 already had.

Three layers because a check in a JSON shim is a single point of failure for
the thing that opens a merge gate, and because layer 3 is the only one a future
caller reaching `record_verdict` by another path would still hit.

The liaison is not thereby mute: it relays what it found with
`message_orchestrator` or `report`. What it cannot do is *settle* anything —
the same boundary the human-question registry draws when it lets every agent
ask and no agent answer (`human-questions.md`).

### `group_usage` is offered

"How is it going" is the question this pane exists to answer, and "what is this
costing" is that question with a number in it. `group_usage` aggregates the
group's tokens and estimated dollars — the figure a human actually asks for
mid-session — and it was `require_orchestrator`-only, so without this rule the
human's own pane has to ask the orchestrator to interrupt its dispatch loop and
relay a number the registry already holds. That round trip is the noise this
whole feature exists to remove.

**What makes it grantable is what the tool is, not who wants it.** It is a READ
of an aggregate scoped to the caller's own group — `caller.group` is resolved
from the token and is never a tool argument — so it reaches nothing outside the
group the pane is already in, settles nothing, and writes nothing. The same
paragraph is the reason no other orchestrator-only tool follows it: `send_prompt`
and `spawn_agent` are orchestration authority, and board writes are durable
state. A widening argued from "the liaison would find it useful" would have
taken those too.

Granted at the two layers this tool has, and keyed on the **conjunction**
`kind: reviewer` **and** `role_hint: liaison`:

1. `mcp::tool_defs` — listed for a liaison-hinted reviewer. Cosmetic, but a
   pane that is never shown a tool never calls it.
2. `mcp::call_tool`'s `group_usage` arm — `require_orchestrator_or_liaison`,
   a function of its own rather than a hint arm inside `require_orchestrator`.
   That one gates roughly twenty tools, including `spawn_agent`, `send_prompt`
   and every board write; a widening written there would widen all of them at
   once. The separate function widens nothing on its own: it is opted into one
   call site at a time, so its blast radius is exactly the arms that name it —
   `group_usage` and, since #1091 slice E, `ask_human`.

There is no third layer, and the absence is structural rather than an omission:
`OrchRegistry::group_usage` takes a group and no caller, because the only
identity it could check is a group the caller is already in. `record_verdict`
has a deepest layer because it is a durable, attributed *write*.

**The conjunction is the fail-closed half, and its asymmetry with the deny above
is deliberate.** A DENY keyed on the hint alone fails closed for every class that
could ever carry it; a GRANT must name the one class it is granting from.
`parse_workflow` already refuses `liaison` on any kind but `reviewer`, so the
class costs a real liaison nothing — it is there for the future producer of a
`Caller` that does not inherit that guarantee.

### `ask_human` is offered (#1091 slice E)

The second widening, and the first that is a **write** — so the paragraph above
that carried `group_usage` ("a READ of an aggregate, settles nothing, writes
nothing") does not carry it, and reusing it would be exactly the borrowing this
section's split is meant to prevent. It needs its own argument.

**What the pane could not do.** A `role_hint: liaison` block is
`Role::Reviewer`, and `ask_human` was `require_orchestrator`-only, so the pane
the human is actually talking to had no way to put a decision into the human's
own durable inbox. Its only route was `message_orchestrator` — which becomes a
registry row **only if the orchestrator independently chooses to open one**.
That is orchestrator-controlled by construction, so as a *liaison* capability it
does not exist: the human's pane could raise a decision and have it evaporate,
which is the failure the registry was built to end (`human-questions.md`).

**What makes it grantable is what the write is.** It appends a row to the
human's own inbox. It settles nothing, releases no work, opens no gate, and
cannot be answered by the pane that opened it or by any other agent — the
registry's boundary was always drawn between **asking and answering**, not
between roles, and every agent already may ask in the sense that matters (a
delegate asks through `message_orchestrator`). What the liaison gains is that
its ask lands in the record rather than in someone else's judgment about whether
to record it. Contrast the writes that were *not* granted and will not be: a
board write is durable orchestration state, `send_prompt` and `spawn_agent` are
authority. A widening argued from "the human's pane would find it useful" would
have taken those too.

**The pose only.** `withdraw_question` is the other half of the same WRITE tier
and stays orchestrator-only, deliberately: withdrawing *settles* a row — any
pending row, not just your own — and the grant buys the human's pane the ability
to add to their inbox, never to decide what leaves it. A liaison whose question
is overtaken by events says so with `message_orchestrator`.

**The answer notice still goes to the orchestrator.** `answer_question` delivers
through `deliver_to_orchestrator` regardless of who asked, and this slice does
not touch that: an answer's consequence is un-blocking a board row, and only the
orchestrator writes the board. The liaison reads what became of its question
from `list_questions` (already on its surface, shared tier), and its own prose
says so rather than leaving it to be discovered.

Granted at the two layers this tool has, keyed on the same **conjunction**:

1. `mcp::tool_defs` — `ask_human_tool()`, one shared definition with two call
   sites, pushed for a liaison-hinted reviewer beside `group_usage_tool()`.
   Shared rather than copied for the reason `group_usage_tool()` is: two copies
   of a description that long drift, and the two panes that can pose a question
   reading different accounts of what makes a good one is the authoring-standard
   split the single funnel exists to prevent.
2. `mcp::call_tool`'s `ask_human` arm — `require_orchestrator_or_liaison`, whose
   refusal names *this* capability ("posing a question to the human") rather
   than the other caller's, since one shared gate with one hard-coded message
   would tell a pane refused `ask_human` that usage aggregation is
   orchestrator-only. The arm's **success** reply is branched on the same
   predicate (`caller_is_liaison`, one function, so the gate and the reply
   cannot disagree): the orchestrator's version tells the caller to mark the
   board row and to expect the answer notice in its own pane, and both are
   false here. A widened gate that left them there would have told the human's
   own pane to wait for something that never comes — the stall the feature
   exists to remove, reintroduced by a string.

There is no third layer, and the absence is structural in the same way
`group_usage`'s is: `OrchRegistry::ask_human` takes the group and an `asker`
string it records rather than checks, because there is no narrower question to
ask of a caller already resolved to this group. The *dangerous* half of this
registry — `answer_question` — has its deepest layer precisely there, and it is
untouched.

**None of the three rules reads anything the agent supplied.**
`Caller::role_hint` is resolved in `resolve_token` from the group's own roster,
via the block recorded on the agent at spawn — the same lookup
`record_verdict`'s deny layer and `idle_reap_candidates` make. There is no tool
argument, pane title or prompt text that reaches any of these decisions.

## Why the exceptions are not a hole in the doctrine

`role_hint` was introduced as **inert with respect to capability**, and four
rules read it today: `session_digest` is offered to `process`-hinted workers
alone, `review_verdict` is withheld from a liaison, and `group_usage` and
`ask_human` are offered to one. The third was the first `role_hint` in loomux
that yields **more** than its `kind` alone; the fourth is the first that yields
a **write**.

So the doctrine is not *hints never grant*. It is **inert by default, with
every exception enumerated here — narrowing and widening alike**, and the table
below is that enumeration. A rule invisible here is the surprise it exists to
prevent.

What a widening does change is the **bar**, not the doctrine. A narrowing needs
only to be safe; a grant has to argue the tool, and the argument that carried
`group_usage` — a caller-group-scoped read that settles and writes nothing — is
the reason it did not carry `send_prompt` or a board write alongside it. Reuse
that argument, not this precedent. `ask_human` is the demonstration: it is a
write, so that argument does not reach it and it had to make its own — the row
it appends is in the human's own inbox, settles nothing, and cannot be answered
by the pane that wrote it. Two grants, two arguments; the second borrowed
nothing from the first but the gate function.

The invariant that genuinely does hold is the one about the **file**, not about
the hint: **a workflow file can never grant a capability**, because a repo
cannot author these rules. It selects a hint from a closed set; loomux decides
what each one means, in code that ships with the binary. That is untouched by a
widening — a repo writing `role_hint: liaison` gets whatever loomux's code says
a liaison is, and cannot add to it.

| Hint | Class | Effect on capability | Status |
|---|---|---|---|
| `advisor` | `planner` | none | shipped |
| `process` | `worker` | **narrows**: `session_digest` offered to this hint only | shipped |
| `liaison` | `reviewer` | **narrows**: `review_verdict` withheld from this hint | shipped |
| `liaison` | `reviewer` | **widens**: `group_usage`, otherwise orchestrator-only | shipped |
| `liaison` | `reviewer` | **widens**: `ask_human` — the pose only; `withdraw_question` stays orchestrator-only | shipped |
| `liaison` | `reviewer` | **no widening**: `request_attention` / `withdraw_attention` stay orchestrator-only — the trip-wire below fired instead | shipped |

**Capability is not the only thing a hint can key on, so the enumeration owes a
second table.** These rules change no tool surface at all — the liaison's tokens
answer exactly as before — but they are read from the hint in loomux's own code
and a reader looking for "everything `liaison` does" would otherwise stop one
table too early:

| Rule | Where | Why |
|---|---|---|
| A merge gate may not name a liaison | `parse_workflow`, and the pane's validator | It records no verdict, so the gate could never open — refused at parse rather than discovered at merge time |
| A liaison is not in the PR fan-out | `is_reviewing_block` → `{{REVIEWERS}}`, a reviewer's "one of N" lane | A PR routed to it is a review that can never complete |
| A liaison is not a class's default block | `block_for` (**Lifecycle**, below) | A bare `spawn_agent(kind: "reviewer")` must not open the human's pane |
| A liaison is never idle-reaped | `idle_reap_candidates` (**Lifecycle**, below) | The reaper cannot see a human typing, so "idle" there means "mid-conversation" |

Four rules and three capability rules is more than the "couple of entries" that
paragraph above named as the trigger to revisit a first-class `Role::Liaison` —
so, deliberately: **the count is not the trigger, the ROOT is.** Five of the
seven follow from one fact stated once — a liaison rides `reviewer` and reviews
nothing — and each is the same three-word predicate at a different site.

**The two grants are the ones that do not**, and this note says so rather than
filing them under the same root. Both follow from a second fact — a liaison
faces the human — which is the first thing about a liaison that is not a
consequence of the class it borrows: a human asks what this is costing
(`group_usage`), and a human has decisions to make later, away from the pane
(`ask_human`). That is worth watching, because "a rule that does not reduce to
the root sentence" is exactly the test named above.

**The second root has now accreted, and this note said that was the trip-wire.**
So the judgment is recorded rather than left implicit. `ask_human` is the second
tool granted from it, and the honest reading is that a `Role::Liaison` moved
from "not needed" to "argued but not yet earned" — the answer is still no, for
two reasons and not one:

- **This tool is the second root's own definition, not a convenience on top of
  it.** "A liaison faces the human" and "the liaison can put a decision into the
  human's inbox" are close to the same sentence; the trip-wire was written
  against tools accumulating *around* the root ("a third and a fourth tool
  granted because the human's pane wants them"), which is a different shape from
  the root's own mechanism arriving.
- **The cost has not moved.** A fifth kind is still ~60 sites, a template and
  four golden fixtures, and both grants are still expressible as one predicate
  at two call sites of one gate function.

**The trip-wire therefore tightens rather than resets: a THIRD tool on the
second root is the trigger**, and the next one that is a *write* is the trigger
regardless of count — because two writes granted to a pane whose whole doctrine
is "holds no orchestration authority" is a class asking to exist, and the answer
then is the fifth kind, deliberately, not a longer table.

**The trip-wire has now fired, and the answer was the one it named** (#1151 slice B).
`request_attention` — the tool that puts a demo or a feedback ask into the human's
NEEDS-YOU queue — arrived as a candidate for the same widening `ask_human` got, and the
plan that specified it (#1151, plan-861) said `require_orchestrator_or_liaison` by
analogy. It hits BOTH clauses above at once: it would be the third tool on the second
root, and it would be that root's second write.

It was **not** widened. The gate is `require_orchestrator`, and the human-facing pane's
raise belongs to `Role::Manager` (#1161), whose own definition cites this trip-wire as
the reason the fifth kind exists at all — so the manager's enumerated tool surface, not
a fourth row on the table above, is where that grant goes. Two things make this cheap
rather than austere: widening later is one word at one call site, while narrowing a
shipped grant is a contract break; and the liaison loses nothing it had, since a raise
it cannot make is one it tells the orchestrator about through `message_orchestrator`,
exactly as it does for `withdraw_question` today.

So the rule this section states is now also a rule this section has been measured
against once, which is the only way to tell a trip-wire from a sentence.

## The prose

**No traffic is rerouted by any of this.** No notice changes destination, no
report is re-addressed, no board write moves — which is what makes the
degradation argument structural instead of a promise: kill the pane and the
group behaves byte-for-byte as it did. That is still true with the capability
rules above in place, and deliberately: `ask_human` gave the liaison a tool, not
a destination — the answer notice for a question it posed still goes to the
orchestrator's pane. The feature is therefore two pieces of prose, and the
reason no goldened role template is touched by either.

**The orchestrator's fragment** — `{{LIAISON_NOTE}}` in `templates/workflow.md`,
produced in `workflow_section` behind `role_hint_block(blocks, "liaison")`, so a
group that declares no liaison reads not one word about one. It carries: start
the pane on the first turn; put questions for the human to it with `send_prompt`
(INVARIANT 2's hold semantics are untouched — only the pane the question is
*asked in* moves); let it serve status itself rather than briefing it; never
forward operational traffic to it; record a directive it relays as a **human**
directive, while never reading a relay as a grant, because it carries the
human's words and not their authority; ask the human directly whenever it is not
alive; and never kill it for looking idle.

**The rule that a relayed directive counts as the human's is keyed on the one
line an agent cannot write.** loomux mints the `[orrerix] message from <id>:`
prefix from the caller's own token; the agent supplies what follows. That half
was interpolated raw, which made the key forgeable by any delegate — so this
slice scrubs it at **every tool a delegate can call to put text in the
orchestrator's pane**: `report` (both shapes, and `ref`/`detail_url` as well as
the note), `message_orchestrator`, and `review_verdict`'s summary. The
mechanism is `notify::sanitize_pane_text`, the function `sanitize_gh_text` has
always been, and `channel_send` has always used it (`orchestration.md`, *#576:
loomux's own notices are not questions*, carries the enumeration).

The claim is scoped to what that buys and no further: a **delegate's** text
reaches the pane carrying no `[orrerix] …` span of its own, so it cannot be read
as a relay it is not. Two things it deliberately does not claim. Text the
*orchestrator* dictates to a delegate is still that delegate's call to make —
the proxy-authorship residual `deliver_relayed_to_orchestrator` already argues,
and unrelated to attribution. And no scrub can decide who *dictated* words a
liaison genuinely relays; that is the fidelity problem the verbatim rule and
the two ledgers address, not a trust-boundary one.

**What the guards cover, stated at the size they are.** The first fix here
closed three fields and left a fourth — `review_verdict`'s summary — because it
was written from the list of paths someone had thought of, so what replaced that
list is two guards. Their reach is worth writing down exactly, because the
temptation is to describe them as "a new field can't get through" and that is
not true:

- **A new notice site** in `mcp.rs` — another `[orrerix] …` composition — is a
  red, from the default-deny source scan.
- **A scrub that stops working** is a red, from the behavioural sweep, the
  `report.rs` unit pins, and the half-dozen older sanitizer tests that share the
  one choke.
- **A new field on a tool the sweep can drive** (free-text or `enum`-constrained
  arguments) is a red: the sweep fills it with a forged span and reads the pane.
- **A new field on a tool the sweep cannot drive** — one with a constrained
  non-enum argument, `review_verdict`'s `pr` being the example — is caught by
  **neither** guard. The scan sees a scrub named somewhere in the call and
  passes; the scrubber itself is unbroken. That one is reviewer-checked, and
  closing it structurally would mean auditing each interpolated *argument*
  rather than the call.

**It presents; it is not the record.** The human-question registry (#946) landed
between this feature's plan and its prose, and the two compose exactly as
`human-questions.md` says they should: `questions.json` is the durable memory of
what the human was asked — it survives a compact, a dead pane and a restart —
while the liaison is a *client* of that record, one of the surfaces that puts a
pending question in front of a human. `list_questions` is on the shared read
tier, so the liaison's own pane can read it. The fragment therefore tells the
orchestrator to keep the durable half durable (the blocked board task, and the
`q-N` where it opened one) and to settle the row itself —
`withdraw_question` — when the answer comes back through the liaison instead of
through an answering surface. Nothing here makes the liaison the holder of a
question, which is the shape #946 rejected on the grounds that a wedged liaison
is a deaf fleet.

**Since #1091 slice E the liaison also WRITES to that record, and the sentence
above is unchanged by it.** Posing a question puts a row in `questions.json`;
the pane still holds nothing, and a liaison that wedges mid-conversation leaves
its questions exactly where the orchestrator and every answering surface can
already see them — which is the property that made the registry the record in
the first place. Both fragments say what the widening does not reach: the
liaison writes no board row, cannot `withdraw_question`, and is not the pane the
`[orrerix] answer to q-N` notice arrives in; the orchestrator's fragment adds
that `list_questions` will now show it rows it did not open, so read the
`asker`.

That last rule is about the **orchestrator's own** kill-idle-panes discipline,
and it now sits beside a matching promise about loomux's reaper rather than a
warning about it: `idle_reap_candidates` skips the hint (**Lifecycle**), so the
fragment says the guardrail skips it too. The two rules together are the whole
of what anything inside the group will do to that pane, which is why the
fragment says so out loud — with the reaper out of the picture, the
orchestrator's own `kill_agent` is the only thing left in the group that can end
it. (Outside it, the human can always close the pane, and a CLI can always die;
the degradation rule above is what covers both.)

**The liaison's own addendum** — `mechanics_core(Role::Reviewer,
Some("liaison"))`, in the non-overridable core for the reason the advisor's and
the process-pro's addenda are: a repo's liaison persona is `mode: replace`-able
and is exactly the half that can forget to say "you hold no authority". The
liaison is also the first hint whose class is wrong about its job — it rides
`reviewer` and reviews nothing — so the addendum says that outright, then
carries: no orchestration authority (never spawn, merge, release, write the
board, or record a verdict; you present questions, the human decides), verbatim
relay with the agent's own commentary kept separate from the quote,
`note_directive` at the moment of receipt, the duplicate-delivery rule, the
read-only tools to answer "how is it going" from, and the half of #946's trust
boundary that lands on this pane in particular — it is the one most likely to be
handed an answer, and it may present a question but never settle one. Since
#1091 slice E it also carries the pose: `ask_human` is yours, never a blocking
interactive dialog (the pane takes no delivery while one is up), for a decision
the human should make later or away from this pane rather than for the one they
are making with you right now — and the three edges of the grant, since a pane
that has to discover them is a pane that waits for a notice that is not coming.

**Where that addendum actually appears** is the same rule as for every other
hint, and it is worth being exact about: a block's instructions file is the core
only when a `mode: replace` persona has dropped the built-in body, and Copilot's
slim system-prompt body always carries it. A liaison block with **no** persona
therefore reads the built-in `reviewer.md` — the addendum is its floor against a
persona that forgets, not a substitute for having one. A liaison persona for
loomux's own workflow file is a separate change.

## Lifecycle

A liaison pane is **started by prose and ended by nothing but the orchestrator's
own decision.** There is no spawner, no supervisor loop and no restart policy;
the two pieces of machinery here are both *subtractions* — a resolution that
skips it, and a reaper that skips it.

### Starting it: the fragment, not a spawner

`{{LIAISON_NOTE}}` tells the orchestrator to open the pane on its first turn if
`list_agents` shows none running, by block id and with a task. That is the whole
spawn path, and it is deliberate: a code path that spawned a liaison would be a
second way to open a pane, would need its own answer for "what if the human
killed it on purpose", and would be the first thing in the group whose existence
did not trace back to an orchestrator decision.

Spawning it by **id** matters — see the skip below — and so does spawning it with
a **task**, which is what keeps it off the idle clock at birth (a pane spawned
with an empty task starts that clock immediately).

### A bare `kind: reviewer` spawn never resolves to it

`spawn_agent` may name a `kind` instead of a `block`, and with no block
`spawn_agent_ex` falls to `block_for(role)` — "the first block of that kind in
roster order". A liaison is reviewer-KIND, so a roster that declared it before
its reviewers parsed completely clean and still answered
`spawn_agent(kind: "reviewer")` with **the liaison**: a pane holding a
reviewer's instructions, no `review_verdict`, and no way to satisfy the gate it
was spawned for. It failed **closed** — no verdict is forged and the gate simply
stays shut — which is what made it a usability trap rather than a security one,
and what made it safe to leave to this slice instead of smuggling a behaviour
change into the one that introduced the hint.

`block_for` now asks `is_reviewing_block` for a reviewer-kind resolution: the
same predicate the `{{REVIEWERS}}` fan-out and a reviewer's "one of N" lane ask,
so *which blocks review* has one answer across every surface that means it.
`advisor` and `process` need nothing of the sort, because neither takes anything
away from its class; the liaison is the first hint that can make "the first block
of that kind" a pane structurally unable to do the job the class was asked for.

Two consequences, both intended:

- A roster whose **only** reviewer-kind block is the liaison has no default
  reviewer at all, so a bare reviewer spawn is refused rather than redirected —
  and the refusal names the block it skipped, because "this group's workflow
  declares no reviewer block" is flatly wrong to an author looking at
  `kind: reviewer` in their own file.
- Naming the block explicitly is untouched. This resolves a **class** to its
  default; the liaison is simply never a class's default.

### It is never idle-reaped

`idle_reap_candidates` skips a liaison-hinted block. The reaper's premise —
which it audits, in as many words, as *a slot the orchestrator wasn't using was
reclaimed* — is false for the one pane whose user is not the orchestrator.

Everything that clears an idle clock is machine-side: a task at spawn,
`send_prompt` from the orchestrator. **A human typing into a pane clears
nothing**, and there is no signal that it could clear — the raw PTY path the
human types on is not instrumented, and instrumenting it would mean treating
keystrokes as orchestration events. So a liaison stamps its own clock the moment
it `report`s `done` or `blocked` — which for this pane is the moment the
conversation *starts*, not the moment it ends — and everything after that is
invisible. The pane would be killed mid-sentence, and the notice would go to the
other pane.

The cost argument the reaper exists to serve does not carry it either: an idle
pane spends nothing, and the slot it holds is the deliberate v1 position below.
The hint is read from the group's own roster via the agent's recorded block —
never from anything the agent supplied — which is the same source
`record_verdict`'s deny layer reads.

This is the reaper's second exemption, and the two are the same shape: the
orchestrator is exempt because it is the group's root, the liaison because it is
the human's. Nothing else is, and a hint-keyed exemption is not a precedent for
one — the argument here is specifically that *the idle signal cannot see this
pane's user*, which is true of no other block.

### The slot it holds, and the cap

**A liaison occupies a `max_agents` slot, and that is the v1 position rather
than an oversight.** Exempting it from the cap is code the degradation story does
not need, and a cap that silently admitted one more pane than it says would be a
worse trade than a roster sized for what it declares. Size the roster **+1** when
adding a liaison to an existing group.

The launcher's advisory already does most of that arithmetic: `recommend_capacity`
counts every reviewer-kind block, liaison included, so the **recommended** cap has
the slot in it. `minimum` depends on whether a gate is declared, and the
distinction is worth stating rather than rounding off: it budgets
`reviewers_needed`, which is the **gate's** requirement when a merge gate exists —
and a gate can never name a liaison, so the slot is excluded — but falls back to
the raw reviewer-kind count on a **gateless** roster, where it is therefore
included. Both answers are defensible (a gateless roster has no smaller number to
give), and neither changes the +1 guidance above, which is about the cap a human
sets rather than about the advisory.

What is left is a **noun**, not a number: `extra_tiers` can render the liaison's
slot as "1 more reviewer". Settled here rather than deferred again: **no change.**
The count a human sizes their cap from is right, the word is cosmetic, and
teaching the capacity advisory a per-hint vocabulary would put liaison-specific
copy in the launcher to fix a sentence that is off by one word. If it ever
misleads someone, it is a launcher-copy fix and not a capacity one.

## What is still not shipped

- **No user-docs page.** `docs/` documents what a human operates; that page is
  its own slice, now that the lifecycle it would describe is settled. The
  authoring skill's field table lists the value because that table is the *parse
  contract* and a parse contract that omits an accepted value is wrong; it
  deliberately carries no "how to build a liaison" recipe.
