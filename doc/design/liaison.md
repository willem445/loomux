# The liaison block (`role_hint: liaison`)

A liaison is the pane a **human** talks to. It converses in natural language,
reads the group's board and state to answer "how is it going", presents the
orchestrator's questions for the human, and relays the human's intent back. It
holds no orchestration authority of its own.

This note covers the foundation: the `role_hint: liaison` value itself — the
public `.loomux/workflow.yml` surface it adds — and the one capability rule
keyed to it. The liaison's prose (its mechanics addendum and the orchestrator's
`workflow.md` fragment), its lifecycle, and the user-facing documentation are
separate slices and are **not** described here as though they shipped.

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
and four golden fixtures, and buy no capability the reviewer class plus one
narrowing rule cannot already express. If the exception list below ever grows
past a couple of entries, *that* is the trigger to revisit — not aesthetics.

## The one hint-keyed rule: `review_verdict` is withheld

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

## Why a narrowing exception is not a hole in the doctrine

`role_hint` was introduced as **inert with respect to capability**, and this is
the second rule to read it (after `session_digest`, which is offered to
`process`-hinted workers alone). Both of the rules that exist **today** narrow;
neither grants. So the strongest thing a repo achieves right now by writing
`role_hint: liaison` is to receive **less** than the class it named would
otherwise give it.

**That is a statement about today, not an invariant, and it must not be written
as one.** The liaison track's own next slice plans a hint-keyed *grant*:
`group_usage` is `require_orchestrator`-only, so offering it to a liaison would
be the first `role_hint` that yields **more** than its `kind` alone. Whether
that is the right trade is that slice's argument to make — but a note claiming
no such combination can exist would force that slice to retract this page
before it could even open the question, which is precisely how a doctrine
hardens into an obstacle instead of a rail.

The accurate statement is therefore *inert by default, with **every** exception
enumerated here — narrowing and widening alike*. The invariant that genuinely
does hold is the one about the file, not about the hint: **a workflow file can
never grant a capability**, because a repo cannot author these rules. It
selects a hint from a closed set; loomux decides what each one means, in code
that ships with the binary.

| Hint | Class | Effect on capability | Status |
|---|---|---|---|
| `advisor` | `planner` | none | shipped |
| `process` | `worker` | **narrows**: `session_digest` offered to this hint only | shipped |
| `liaison` | `reviewer` | **narrows**: `review_verdict` withheld from this hint | shipped |
| `liaison` | `reviewer` | **widens**: `group_usage`, otherwise orchestrator-only | *planned, not shipped* |

Keep the last row until the slice that lands it turns it into a shipped one, or
until it is decided against and the row is deleted. A planned widening that is
invisible here is exactly the surprise this table exists to prevent.

## What this slice deliberately does not ship

- **No prose.** There is no liaison `mechanics_core` addendum and no
  `{{LIAISON_NOTE}}` fragment teaching the orchestrator to route questions to
  it. A liaison block declared today spawns a reviewer-class pane with a
  reviewer's instructions, a LIAISON badge, and no verdict tool.
- **No lifecycle work.** Nothing spawns a liaison automatically, and whether a
  pane that never calls `report` survives the idle reaper is an open question,
  not an answered one.
- **No user-docs page.** `docs/` documents what a human operates, and the
  operable feature is the prose plus the lifecycle, neither of which exists
  yet. The authoring skill's field table lists the value because that table is
  the *parse contract* and a parse contract that omits an accepted value is
  wrong; it deliberately carries no "how to build a liaison" recipe.

### A bare `kind: reviewer` spawn can resolve to the liaison

`spawn_agent` may name a `kind` instead of a `block`, and with no block
`spawn_agent_ex` falls to `block_for(role)` — "the first block of that kind in
roster order". A roster that declares its liaison before its reviewers, with a
gate naming a real reviewer, parses completely clean and still answers
`spawn_agent(kind: "reviewer")` with **the liaison**: a pane holding a
reviewer's instructions, no `review_verdict`, and no way to satisfy the gate it
was spawned for.

It fails **closed** — no verdict is forged and the gate simply stays shut — so
this is a usability trap, not a security one. `advisor` and `process` share the
same default-block shape harmlessly, because neither takes anything away; the
liaison is the first hint that can make "the first block of that kind" a pane
structurally unable to do the job its class was asked for.

The fix is a `block_for` skip, and it belongs with the lifecycle slice that
already owns how a liaison is spawned — not here, where it would be a behaviour
change smuggled into the slice that introduces the hint. Until then: name the
block explicitly when spawning a reviewer into a roster that declares a liaison.

### One known imprecision, left deliberately

`recommend_capacity` and `extra_tiers` count reviewer blocks by `kind`, so a
liaison counts as a reviewer in the launcher's capacity advisory. The **number
is right** and stays right: a liaison is a live pane and does occupy a
`max_agents` slot, which is the v1 position — exempting it is code the
degradation story does not need. What is imprecise is only the **noun**: a
roster carrying a liaison can read "1 more reviewer" where the extra slot is
the liaison.

`reviewers_needed` is unaffected in the case that matters, because a gated
roster derives it from the gate's own reviewer list, which can never name a
liaison. Naming the liaison separately in that advisory belongs with the
`max_agents` question the lifecycle slice already owns, not here — a capacity
refactor riding along in the slice that introduces the hint would make both
harder to review.
