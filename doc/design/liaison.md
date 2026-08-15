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
rather than a residue: read-only containment (a read shell plus `git`/`gh`, so
it can read the audit log and the board without poking the orchestrator),
persistent (no auto-close), channel-eligible, and already able to
`report`/`message_orchestrator` — which is the whole downward wire. Nothing new
is invented for it: no dependency, no MCP tool, no persisted state.

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
`process`-hinted workers alone). Both narrow; neither grants. The invariant
that matters — *a workflow file can never grant a capability* — is untouched,
because the strongest thing a repo can achieve by writing `role_hint: liaison`
is to receive **less** than the class it named would otherwise give it. There
is no combination of `kind` and `role_hint` that yields more than `kind` alone.

The accurate statement of the doctrine is therefore *inert by default, with
every exception enumerated*, and this is the enumeration:

| Hint | Class | Effect on capability |
|---|---|---|
| `advisor` | `planner` | none |
| `process` | `worker` | narrows: `session_digest` is offered to this hint only |
| `liaison` | `reviewer` | narrows: `review_verdict` is withheld from this hint |

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
