# Human questions — asking without blocking the fleet

*#946. This note covers slice Q1: the registry, its MCP tools, and the trusted
answer surface — and, in its own later section, Q4 / #1091 slice H: the
structural deny of the blocking dialog and its latched-attention belt. The
inbox UI (Q2), the orchestrator's protocol prose (Q3) and the chat bridge
(#947 / T1) still sit on top of what is described here and extend this note as
they land.*

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

## Q4 / #1091 slice H — the structural deny, and its per-CLI coverage

*This section is deliberately its own, distinct from anything #1091 slice F
adds elsewhere in this note on `integration/ui-redesign` — the two land on
different branches and must merge without touching each other's prose.*

Q3's protocol prose (and #1091 slice E's widened version of it, for
`liaison.md` too) says "use `ask_human`, never a blocking dialog." Prose is
instruction-backed, not structural — the whole reason this note exists is that
an instruction did not stop the #578 incident. Q4/H is the enforcement half:
make the blocking dialog **unreachable** for the two roles whose pane a hold
can stall a fleet behind, rather than merely discouraged.

### The deny predicate

`claude_denies_interactive_question(role, role_hint) -> bool` —
`role == Orchestrator OR role_hint == "liaison"` — is **orthogonal to
`Containment`**, not a fourth tier on that ladder. `Containment` answers "may
this agent edit files / mutate git"; this answers a different question
entirely ("may this agent's CLI hand control to a dialog that blocks the whole
pane"), and the two happen to compose in the same `--disallowedTools` value
list on Claude only because that is where Claude's CLI puts every tool-level
deny.

- **The orchestrator** is `Containment::None` (denies nothing today) and still
  gets this deny — the one case where `--disallowedTools` opens on an
  orchestrator's command line at all.
- **A liaison-hinted reviewer** (`kind: reviewer` + `role_hint: liaison`,
  #891) is `Containment::NoEdits` and gets both denials in the **same**
  `--disallowedTools` flag — Claude Code does not merge two occurrences of
  that flag on one command line, so the deny is *extended* into the
  already-open list for a liaison, never opened a second time.
- A worker, a planner, and a plain (non-liaison) reviewer never get this deny:
  a human standing at a delegate's own pane, answering its dialog in person,
  never stalls anyone else — the incident this closes is specific to the two
  roles other agents route their reports and questions through.

A group with no liaison block simply never asks the predicate the question
that would return `true` for one — no special case, no error. Nothing here
adds a dependency on a liaison existing, which is the #891 principle this
slice inherits rather than re-derives.

### Per-CLI coverage — implemented vs. documented gap

Only **Claude** gets code in this slice: `CLAUDE_QUESTION_DENY_TOOLS =
["AskUserQuestion"]`, emitted via `--disallowedTools` from both
`build_agent_command_ex` (the shell-line form) and `build_agent_argv_ex` (the
structured argv form spawned directly), pinned against `KNOWN_CLAUDE_TOOLS`
the same way `CLAUDE_EDIT_DENY_TOOLS` is (#448 discipline) so a typo or an
upstream rename fails CI instead of silently denying nothing.

The other three CLIs this repo supports (`SUPPORTED_CLIS`: `copilot`,
`gemini`, `opencode`) are **not** given code here, and that gap is stated
rather than left implicit:

| CLI | Interactive-question tool? | Coverage |
| --- | --- | --- |
| **claude** | `AskUserQuestion` | Structural deny (this slice) |
| **copilot** | None named in `KNOWN_COPILOT_DENY_CATEGORIES` (the CLI's own docs enumerate exactly three `--deny-tool` value shapes — `shell(COMMAND)`, `write`, `MCP_SERVER_NAME(tool)` — none of which names an interactive-choice tool) | Belt + prose only |
| **opencode** | Its containment seam is a generated permissions document (`edit`/`bash`/git patterns), not per-tool names; no interactive-question entry exists in that vocabulary | Belt + prose only |
| **gemini** | **`ask_user` is a real, known tool** — `KNOWN_GEMINI_TOOLS` already lists it (fetched from Gemini's own tool reference, the same snapshot discipline as the Claude list) | **Not implemented here** — see below |

The Gemini finding is worth being explicit about rather than filing away
silently: `ask_user` is exactly the kind of tool `GEMINI_EDIT_DENY_TOOLS`
already denies siblings of (`write_file`, `replace`) via the generated
`policy.toml`, so a future slice could plausibly deny it the same way. It is
not done here because that is a second full builder path
(`gemini_policy_toml`/`gemini_settings_json` generate a **file**, not argv —
a different emit surface from Claude's, with its own tests) and Q4's own scope
names only Claude. Left as a follow-up, not faked as covered.

For every CLI without a structural deny — including Codex, which is not
currently in `SUPPORTED_CLIS` at all but was the CLI the original #1091
plan-478 design named as having no tool-level deny mechanism whatsoever — the
belt below is what actually catches a stalled fleet.

### The latched-attention belt

A CLI-level deny only ever covers CLIs that *have* one. `attn_question_held`
(an `OrchRegistry` field: agent ids currently holding delivery on
`HeldReason::InteractiveQuestion` for their own pane) is the visibility net
underneath it: whenever `deliver_now` holds a delivery because a live
interactive dialog is on screen, and that pane belongs to the **orchestrator**
specifically (never a delegate — a delegate's own dialog, answered by a human
in person, never stalls anyone else), the hold is mirrored into this latched
set via `OrchRegistry::latch_question_held` / `unlatch_question_held`.
`attention_tick` reads it as a new reason, `held-dialog`, ranked **above even
`blocked`** — because a held orchestrator pane strands every other agent's
report behind it too, not just its own status, which is the literal #578
failure mode.

Latched, not re-derived each 3-second scan, because a hold can run for
`QUESTION_HOLD_MAX` (minutes) — long enough to fall between two scans if the
reason were computed fresh each tick the way `waiting` is. It clears the
instant the hold itself clears (`emit_held_cleared` fires unconditionally when
any hold this function entered ends, whatever the outcome), so it cannot
outlive the condition it describes the way `stranded` deliberately can.

**Disjoint from #1091 slice D's `question` attention reason by construction,
not by convention.** D's reason is *derived* from the engine's `ask_human`
registry (`questions.json` — a question the orchestrator posed and is waiting
to be answered; no pane involved, no hold, no delivery pipe). This belt's
reason is about the delivery pipe itself being physically held on a dialog
currently on screen — a different subsystem, a different signal, landing on a
different branch (`feat/1091-question-attention` vs. this slice's
`feat/946-question-deny`, both merging into different targets). The two
reasons sit beside each other in `attention_tick`'s match arm and in the
frontend's `AttentionReason` union; nothing merges them.

The frontend mirrors the reason in `src/attention.ts` (`held-dialog`: label,
urgent — the priority its own doc lists it above `blocked`) and
`src/tabroute.ts` (the same urgency + a `REASON_PRIORITY` entry above
`blocked`'s), consistent with how `stranded` is mirrored in both files.
