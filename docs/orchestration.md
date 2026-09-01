---
title: Orchestration guide
layout: default
nav_order: 4
---

# Orchestration guide
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

Orrerix's headline feature is a native **orchestrator / worker** pattern: a
long-lived planning agent that manages a small fleet of worker agents, each in
its own visible pane, with a reviewer agent per PR and an optional **planner**
that scopes bigger work first. You gatekeep only the final review and merge.

Every agent is a normal CLI in its own pane — **panes, not subagents** — so you
can watch and steer any of them by typing directly, and every inter-agent prompt
is delivered by *typing into the recipient's CLI*, visible verbatim and captured
in an audit log.

## Launching a group

1. Open a new pane (split or new tab) and pick the **Orchestrator + workers**
   kind on the welcome / pane-setup screen.
2. Choose the agent CLI and model **per role** — orchestrator, worker, reviewer,
   and planner each get their own CLI (Claude Code or Copilot CLI) and model, so
   you can mix agent types in one group (e.g. a Claude orchestrator driving
   Copilot workers). The top *Agent* select is the group default that seeds every
   role; override any role you like. Model dropdowns are populated by asking the
   selected CLI what it offers — `opencode models`, which lists the models *your*
   configured providers actually expose, for OpenCode; the CLI's own help for the
   others — so new models appear automatically, with a custom-entry escape hatch.

   Orrerix also asks each CLI directly which models *this machine and this
   account* can actually run, and what each one supports — the same list your
   CLI's own model picker would show. What comes back adds rows to the dropdown
   (nothing is ever removed), labels each with the CLI's own name for it, and
   fills in a line under the control with the model's description, its
   reasoning-effort levels, and its context-window size. The effort levels the
   CLI reports for the selected model then become the ones the **thinking
   level** knob offers.

   This happens **automatically, once, when orrerix starts** — there is nothing
   to click. Detection runs in the background, so a picker you open in the first
   seconds may show its built-in suggestions for a moment and fill in the real
   list as the answer arrives. A CLI you install *while orrerix is running* is
   not detected until you restart it.

   **Copilot CLI is the exception on both counts.** It has no supported way to
   list its models — its help no longer enumerates them, and it answers neither
   of the questions above — so its dropdown is a built-in catalog of the models
   Copilot offers, kept in orrerix rather than read from your machine.

   That list is Copilot's product catalog, not your account's, and it can go out
   of date between orrerix releases. So it can disagree with what you can
   actually run **in both directions**: it may offer a model your plan does not
   include — picking one fails at launch rather than being hidden, which is
   deliberate, since orrerix would otherwise have to guess your entitlements —
   and it may omit a model your plan *does* include, whether because your
   account has access the general catalog does not list or because the model is
   newer than your build of orrerix. **The custom-entry box is the answer to
   every one of those cases:** type the id and it is used as-is. If your Copilot
   CLI offers a model this dropdown does not, that is expected rather than a
   sign it is unavailable to you.

   Every other CLI keeps the automatic behaviour described above, and Copilot
   will too once it gains a way to be asked.

3. Set the repository and the guardrails: **max live agents**, the cost and
   recovery limits (idle-kill, max spawns/hour, watchdog stall), and
   **permissions**.

   A group starts with **no workers**, and there is no setting for it. The
   orchestrator opens the ones the work needs once it has read the issue —
   before that, any number would be a guess, and idle agents cost tokens. Ask
   it for more (or fewer) at any time by typing into its pane.

The card shows the **mark of the agent CLI** it is about to launch, beside the
title: GitHub's own Copilot glyph for Copilot CLI, a lettered badge for the
CLIs with no licensed mark, and nothing at all for a pane that runs no agent —
a terminal, a file explorer, or an SSH connection with no remote CLI chosen.
It is drawn in that CLI's own colour, and it is the same mark the pane header
wears once the pane is running.

### Thinking level and context window

Beside each role's CLI and model select, the launcher offers two more
per-role knobs: **thinking level** and **context window** — three knobs in
total (model, thinking level, context) for each of orchestrator, worker,
reviewer, and planner. All three default to **CLI default** — the empty
value, which orrerix emits nothing for, so the CLI runs exactly as it would
with no flag at all.

- **Thinking level** sets how hard the model reasons before answering
  (`low`/`medium`/`high`/`xhigh`/`max`), on Claude Code only. Copilot CLI and
  Gemini CLI grey the control out with a reason shown inline: Copilot's
  effort level lives in `~/.copilot/settings.json` with no flag or
  environment variable to set it, and orrerix never writes a user's global
  settings file to reach it; Gemini's thinking level is a settings-file key
  too (`modelConfigs.aliases.<alias>.thinkingConfig`) — the seam orrerix uses
  to generate a per-agent Gemini settings file already exists, but the key's
  schema needs a live check against a running Gemini CLI, so the knob stays
  disabled until that's verified.
- **Context window** widens the model's context (`1m`, the only tier today),
  on Claude Code only, composed onto the model as the `[1m]` alias suffix
  (`sonnet[1m]`) rather than typed into the model field itself. Copilot's
  context control is interactive-only (`/context` inside its own session)
  with no argv or settings equivalent, and Gemini's window is
  model-determined, so both grey out too.

A role's knob greys out for either of two separate reasons, each stated
inline as the control's own hint — never silently ignored:

- the **CLI** can't honor the knob at all (the Copilot/Gemini cases above);
- for **context** specifically, on Claude Code, the **selected model** has
  no `[1m]` form. The suffix is documented only for the `sonnet`, `opus` and
  `opusplan` families — `haiku`, `fable`, `best` and `default` each grey out
  with their own reason (there is no `haiku[1m]` or `fable[1m]`; `best` and
  `default` resolve per account, so there's no fixed name to append the
  suffix to). A model id orrerix doesn't recognize — a full model name it
  hasn't seen, or a Bedrock/Vertex/Foundry deployment name — leaves the knob
  **enabled** instead: orrerix only disables what it can affirmatively rule
  out, never what it merely doesn't know, since on those providers the
  suffix is exactly how the 1M window gets selected.

A knob that clears both checks is still an entitlement, not a guarantee:
`opus[1m]` is a real, documented alias, but `[1m]` access is plan- and
credit-gated on Claude's side, so picking it for an account that can't serve
it fails visibly at the CLI, in the pane — orrerix doesn't pre-judge your
account's entitlements by hiding the option. That's a different failure from
the model gate above: the model gate hides a suffix that has no defined
meaning at all, while the entitlement case leaves a meaningful suffix
selectable and lets the vendor's own check decide.

These same two keys are available per block in `.orrerix/workflow.yml`
(`effort:`/`context:`) for the advanced orchestrator. Loading the file
enforces the closed vocabulary and the per-CLI rule above; the workflow pane
goes further and also validates `context:` against the block's `model:`,
raising a per-block finding when the two disagree (e.g. `model: haiku` with
`context: 1m`) — the same model-gate rule the launcher's select uses, so a
hand-edited file can't drift from what the launcher would show. In the
pane's block form the two controls follow the model **as you change it**,
including a model id you type by hand: pick `sonnet` over `haiku` and the
context window becomes selectable in the same keystroke, with no need to
click away from the block and back. See
[`doc/design/workflows.md`](https://github.com/willem445/orrerix/blob/main/doc/design/workflows.md)
and the `author-loomux-workflow` skill.

**Permissions** are either *Auto* (Claude Code's native auto permission mode plus
pre-approved `git`/`gh` and orrerix agent tools — recommended) or *Accept edits
only*. Orrerix never uses `--dangerously-skip-permissions`.

Under *Auto*, **group Copilot** agents run in Copilot's true **autopilot mode**
(`--autopilot`) — an unattended worker should persist autonomously rather than
pause to ask — and orrerix answers the resulting "Enable autopilot mode" consent
dialog for them automatically at spawn (your group-level *Auto* choice is the
consent). A lone Copilot pane launched with the **Autopilot** checkbox on gets
the same flags and the same dialog-answering watcher — see
[getting started](getting-started.html#your-first-agent-pane).

The launcher warns inline when any selected role's CLI isn't installed, and an
agent pane that dies with an error stays open so you can read what happened.

### Promoting a standalone agent

Sometimes the group starts as a conversation. You open a plain **Agent** pane to
try something out, spend an hour on it, and it turns into work worth
orchestrating — at which point launching a *separate* orchestrator means
hand-transferring everything you just worked out into a session that wasn't
there for it.

Instead: **right-click the pane's header → "Promote to orchestrator…"**. That
pane's own Claude session is relaunched in place with the orchestrator's full
contract — its tools, its task board and audit log, the git/gh shim, a real
group on disk — while keeping the conversation it already has. The prototype
context *is* the orchestrator's context; nothing is summarized or re-typed.

What the confirm tells you before anything happens:

- **The repository** is the pane's own working directory. A pane launched into a
  worktree keys its group to that worktree, not to the main clone.
- **This pane's current turn is interrupted.** Promotion has to stop the running
  CLI to reopen the session under the new contract, so finish (or don't mind
  losing) whatever it is mid-answer.
- **Which group you land in** is decided when you confirm: a new group for the
  repo; or the repo's existing **dormant** group *reattached*, inheriting its
  board, audit history and the roster it was launched with; or a **sibling**
  group beside one that's already live (two orchestrators never share a group).
  A toast names the group once it resolves.
- **The workflow checkbox** appears only when the repo declares a
  `.orrerix/workflow.yml` **that validates**, and runs that roster instead of the
  built-in four roles. If the file is there but broken, you're told so and
  there's no checkbox: a new group runs the built-in roles, the same outcome the
  launcher warns about inline. Either way — valid file, broken file or no file —
  a **reattached dormant group keeps the roster its own launch approved**; which
  roster a promotion runs depends on the group case as much as on the file.

The item is offered on standalone **Claude** agent panes. It's greyed with the
reason on an agent pane that can't be promoted *yet* — a non-Claude CLI, a pane
orrerix hasn't learned a session id for (send it a prompt first: an agent nobody
has spoken to has no conversation to carry over), or a pane with no working
directory to make the group's repo — and it isn't offered at all on a shell pane,
a pane running something that isn't an agent CLI, or a pane that already belongs
to an orchestration group.
orrerix also refuses a session that was *ever* a recorded member of a group, even
a long-dormant one: a delegate's transcript carries a delegate's contract, and
two role contracts in one session is not a thing to seat on purpose. Every
refusal says which one it is, and a refused promotion changes nothing at all —
the pane keeps running exactly as it was.

If the relaunch itself fails after the old process was stopped (a spawn that
doesn't come up, a bind that times out), orrerix does **not** quietly start a new
session in its place — that would look like success while discarding the very
thing you promoted. The group is already durable on disk, so the toast names it
and points you at its **Resume** card in the session browser, which brings the
conversation back.

Promotion is human-only, like every other gesture on this menu: no agent can
promote a pane, its own or anyone else's.

## How it works

Orrerix hosts a local **MCP server**; every agent pane in a group connects with
its own identity token (`--strict-mcp-config`, so workers see nothing else). The
orchestrator:

- plans work as GitHub issues, labeling ones it owns **`agent-managed`**;
- **every worker AND reviewer spawn gets its own dedicated git worktree — always.**
  Your main clone is *your* environment, so neither ever branches, commits, or
  checks anything out there: a worker's worktree branch is cut from the repo's
  default branch (fetched fresh from origin), never from whatever the primary
  checkout happens to sit on, so parallel work starts from a clean base without
  a manual rebase; a reviewer's own worktree is the same kind of clean scratch
  space, kept separate so two reviewers (or a reviewer and the orchestrator's
  own git traffic) never contend on the same checkout. A reviewer's worktree isn't a checkout of the PR
  it's reviewing (that branch may already be checked out elsewhere); it fetches
  the PR's code in **detached-HEAD** mode when it needs to run something
  locally, which never collides with anything. The orchestrator cannot spawn
  either into the main clone even if it tried — the MCP tool rejects it
  outright. (A planner is unaffected: it never gets a worktree at all — see
  below. For its own mechanical git work, like a rebase or conflict fix with no
  worker worktree still around, the orchestrator uses a **staging worktree of
  its own**, kept separate from your clone the same way.)
  **`git stash` is repo-wide, not per-worktree** — the stash stack lives in the
  shared `.git`, so agents in separate worktrees of the same group share one
  stack and a `pop`/`drop`/`clear` by one can destroy another's WIP; role
  templates tell agents to commit WIP to their own branch instead of stashing;
- delegates via tools that *type prompts into the worker's CLI* — you see every
  instruction verbatim in the pane, can steer any agent by typing yourself, and
  everything lands in the audit log.

Workers follow the standard flow (**branch → implement → tests that test intent
→ docs → PR**) and report back; reviewers post `gh pr review`s. For bigger or
sprawling work the orchestrator can spawn a **planner** first — a read-only agent
that explores the codebase and posts a structured implementation plan (scope,
files, test strategy, risks, and a suggested worker split) as an issue comment,
then exits. A planner's read-only contract is enforced at the CLI level where
possible: it never gets a worktree, and its file-editing tools plus `git
commit`/`git push` are denied.

What it *is* pre-approved for, since a planner runs with no human in its pane to
approve anything: read-only shell and `git`, `gh` (it reads the issue and posts
its plan through it), the orrerix tools, and — so it can ground a plan in a
vendor's actual reference docs rather than in recall — `WebFetch`/`WebSearch`.
That last pair means a planner pane can reach arbitrary hosts, which is worth
knowing if you plan on sensitive repositories; to switch it off, add a `WebFetch`
(or `WebSearch`) entry to `permissions.deny` in the repository's own
`.claude/settings.json` — a deny rule there beats anything orrerix pre-approves.
Running a build or test command is *not* pre-approved, and orrerix offers no way to
widen that — a planner's persona `allow:` patterns are dropped, unconditionally.
Your own `.claude/settings.json` can still add one if you decide to: permission
rules merge across scopes rather than override — the same merge rule that makes
the `WebFetch` switch-off above work. So what orrerix *denies* there is no way to
allow, but what it merely leaves out of its allow-list — general `Bash`, and so
`cargo check` — a repo-level `permissions.allow` can grant. Absent that, a plan
will say when it could not confirm something by running it.

**What actually reaches the orchestrator's pane.** A delegate's report is typed into
the orchestrator's pane — waking it for a turn on the group's most expensive model —
only when it needs the orchestrator to *do* something. `done` and `blocked` do: route
the next step, drive the PR, merge, ask you. `progress` never does, so it is not
delivered at all — it is written to the audit log and appended as a note on that
agent's board task, where you see it beside the pane and the orchestrator can read it
on demand. Nothing is lost; what goes away is the interrupt. A delegate that needs the
orchestrator *now* for something that is not a status change uses its message channel
instead, which always lands.

Every review answers two standing questions as well as reporting what it found. Its body
carries a **`## Premortem`** section — two ways this change fails in production that no test in
the PR would catch, or an argued none — and where the change touches unbounded input (a file, a
transcript, anything off the network or supplied by a user or another agent) one of those two
is the **resource** answer: largest realistic input × how often the code runs × what it
allocates or reads per run, naming the size at which memory or IO hurts rather than only where
time does. Those are *question-generation* duties, and they exist because the rest of the
process is verification: red-before-green evidence proves the tests somebody already thought to
write and is silent about the property nobody conceived of. The orchestrator treats a review
that arrives without the section — or with an unargued "none" under it — as an incomplete
review rather than an approval, and dispositions a premortem entry that names its trigger like
any other finding. A repo that
wants more of the question set than that — design alternatives, misuse, operational futures —
puts it in its own reviewer persona (see **Custom agent workflows**).

**Everything an agent posts on GitHub has two layers.** A PR body, a review, an issue an agent
files: a short human layer first — what changed and why, what to look at, how each finding was
dispositioned — and below it a collapsed `<details>` block, headed *Agent context — evidence,
receipts, instruments*, carrying the evidence the process demands of agents but not of you: run
ids, red-before-green failure lines, mutation tables, blob hashes, base-and-head figures. Click
it open if you want it; skip it if you don't. Nothing about the evidence gets weaker for being
folded — only its position moves — and the standing review sections above stay above the fold,
because they are the part a human most needs to read. When a merge takes its commit message
from the PR body, it takes the human layer only: `git log` has no fold.

**No agent ever merges.** Agents open PRs; you merge, after your own review.

Panes are badged by role and group number (`ORCH 1` / `W 1` / `REV 1` / `PLAN 1`
vs `ORCH 2` / `W 2`) with a per-group accent color, so parallel orchestrations —
even on the same repository — pair up at a glance. When the orchestrator spawns
an agent it opens that pane in the **background**: your keyboard focus stays
exactly where you were typing.

## The label handshake

You can hand the orchestrator work without typing in its pane — just label a
groomed GitHub issue. A running orchestrator on the repo polls open issues and
pulls any so-labelled onto its board; because the label is durable on GitHub, no
orchestrator needs to be running when you label — it's picked up whenever one
next starts on that repo.

| Label | Meaning |
| --- | --- |
| `agent-ready` | Groomed — start work. The issue is driven to a PR through the normal branch → implement → test → PR flow. |
| `agent-investigation` | Research only. A planner (or the orchestrator itself, for small questions) researches options/feasibility and posts findings or a plan as an issue comment — **no code**. |
| `agent-managed` | Set *by* an orchestrator to mark "I own this issue." Shown read-only in the UI. |
| `agent-hold` | The opposite of a go signal: agents must not start this issue. It's the veto that matters under [full autonomy](autonomous-mode.html#full-autonomy-the-orchestrator-picks-the-work), where every open issue is otherwise eligible. This is the only label in this table a repo can rename (`intake.labels.hold:`); do so and your spelling replaces it everywhere, including in the paragraph below and throughout this page. |

You can apply `agent-ready` / `agent-investigation` / `agent-hold` straight from
the [GitHub issues view](features/github-issues.html) — toggle the **ready**,
**investigate** or **hold** control on an issue row. If the repo doesn't have these labels
yet, orrerix creates the one you toggle on first use (only these allow-listed
labels are ever created).

## The task board

The orchestrator pane has a board toggle (`Alt+T` or the list icon) showing the
group's work queue — status per item, issue/PR links, notes, and priority order.
You can add, edit, annotate, reorder, and delete tasks; the orchestrator is
notified of your edits and maintains the same board through its tools. Issue and
PR chips are **clickable** and open in your browser.

Statuses: `queued`, `in-progress`, `review`, `pr`, `human-testing`,
`prototype`, `done`, `blocked`.

### What order the board is in

The order you can drag is the **priority order** — top is next, and the
orchestrator reads it that way. Two things are derived from it rather than
being part of it:

- **Finished work sinks.** An item that is `done`, with nothing unfinished
  nested inside it, drops below the live work **of its own group** (the
  top-level rows, or its container's children — see [Parent tasks and
  subtasks](#parent-tasks-and-subtasks)),
  ordered most recently updated first. Nothing about your priority order
  changes: live items keep exactly the relative order you gave them, and a
  `done` container that still holds an open task stays where it is, so live
  work can never disappear under a finished parent. ("Most recently updated",
  not "most recently finished" — anything that touches an item counts, so the
  orchestrator adding a note to an old finished item lifts it back to the top
  of the finished group.)
- **Cleared items drop out** until you ask for them — see **📥 clear done**
  below.

Because of the sink, the ▲/▼ buttons move an item one step relative to the
items you can *see* above and below it, skipping whatever finished rows happen
to sit between them in the file. On a finished item both arrows are off: its
place is derived (most recently updated first), not yours to set — reopen it and it rejoins
the priority list.

Board controls:

- **▶ Start** on a `queued` item nudges the orchestrator to begin now — it
  records a human note and delivers a *begin work* prompt to the orchestrator
  pane. It deliberately leaves the status at `queued`; the orchestrator flips it
  to `in-progress` when it actually assigns a worker. (If the group is
  **paused**, Start is refused with a toast — resume first.)
- **Merge gate** — when an item reaches `pr` or `human-testing` (the point where
  only you can decide), the board shows **✓ Approve** (marks it done and tells
  the orchestrator to merge) and **✎ Changes** (opens a box for your findings,
  records them, and reopens the task — see below). Both land as a message in the
  orchestrator pane, exactly as if you'd typed it. **Approve only ever shows for
  `pr`/`human-testing`**: once you request changes, the task returns to a working
  status and Approve disappears with it, so a reopened item can never keep
  showing a stale "approve" affordance for feedback you already sent back. Note
  that Approve is *your* merge gate, not the repo's — if a [custom
  workflow](#custom-agent-workflows) has its own merge gate armed, Approve is
  relabeled up front whenever the item carries a PR (e.g. "Approve (won't merge
  — gate needs rev-orch/rev-ui/rev-tests)") so you know before you click, and
  the tooltip names your options: run the missing reviewers, toggle the
  workflow off, or merge via the GitHub UI directly. What the label says
  depends on the PR's **base branch**, which the orchestrator records on the
  task alongside the PR itself:
  - **Base is the default branch** (or the orchestrator recorded no base, or
    orrerix could not resolve the repo's default branch) — the warning above.
    Unknown is treated as "assume the default branch": a board that guessed the
    other way would quietly downplay a merge straight into `main`.
  - **Base is some other branch** — a stacked sub-PR into an integration
    branch, say — the label says so and names it ("Approve (sub-PR into
    integration/581 — the orchestrator merges it once the gate verdicts land)").
    Your Approve grant is the *default-branch* gate, so it is not what this
    PR is waiting on.

  This narrows the **story**, not the gate. A custom workflow's merge gate
  applies to every merge of a PR wherever it lands, integration branches
  included, and it is enforced against the base ref orrerix resolves live at
  merge time — never against what a task says. The recorded base is display
  metadata the orchestrator writes, so treat it the way you'd treat any other
  board text: informative, not authoritative. Two ways the label can be
  *wrong-but-harmless*, both worth knowing: the orchestrator can record a stale
  base if it retargets the PR without updating the board, and orrerix reads your
  repo's default branch from the clone's own refs rather than fetching, so a
  default branch renamed on the remote reads as the old name until something
  fetches. Either way the worst case is a sentence that misdescribes the PR —
  no merge is authorized by any of this.
- **▶ Proceed** on a `prototype` item (a demo-gated deliverable awaiting your
  verdict) promotes it: two-click confirm flips it to `in-progress`, records
  your decision, and prompts the orchestrator to take the prototype to a full
  production build.
- **📥 clear done (N)** clears every finished item out of the list in one
  click. **Nothing is deleted.** Each item keeps its place, its notes and its
  links in the group's board file, the action is recorded in the audit log,
  and it comes straight back:
  - **👁 show cleared (N)** puts them back on screen (they read dimmer, and
    each wears a small *cleared* label saying when you cleared it),
  - **↩** on any one of them brings that one back into the working list, and
  - **↩ restore all (N)**, next to the toggle while they are on screen, brings
    back the lot.

  Clearing is *your* view of *your* board: no agent can clear an item, and no
  agent can see that you cleared one — the orchestrator's own view of the
  board is byte-for-byte what it was. Reopening a cleared item (moving it off
  `done`) brings it back on its own, so a task the orchestrator picks up again
  can never stay hidden. Because it is a view action and not a work change,
  the orchestrator is *not* interrupted with a notice about it, the same way
  it isn't for a reorder.
- **🗑 done (N)** deletes all `done` items in one action (two-click confirm).
  This one is permanent and it does **not** spare the cleared ones — the count
  is every `done` item on the board.
- **🗑 selected (N)** deletes exactly the rows you tick, by id, in one action.
- **✓ Approve selected (N)** approves several merge-gate items at once, using
  the same tick boxes. The count is only the ticked rows that are actually at
  the gate — tick a `queued` row for a later delete and it is simply not part
  of the approval. One dialog lists exactly what you are about to authorize,
  with an optional note per row (they ride to the orchestrator attached to
  their own PR), and that dialog is the confirm: nothing is granted until you
  click through it. Each PR still gets its **own** one-time grant, exactly as
  if you had clicked Approve on each row — what the batch changes is that the
  orchestrator gets **one** message naming every approved PR and your notes,
  instead of one prompt per PR. The batch is all-or-nothing: if a row moved off
  the merge gate between the board rendering and your click, or two of the rows
  you ticked point at the *same* PR (a duplicate filing — one grant can't be
  announced as two), the whole batch is refused with a toast and nothing is
  granted; re-tick and click again. A grant that fails to *write* (a full disk)
  likewise stops the action with an error rather than quietly reporting that
  item as having had no PR.

Items that only you can advance (`pr`, `human-testing`, `blocked`) are
highlighted so what's waiting on you stands out. A working-status item
(`in-progress`/`review`) is one of two things, and the board makes the
difference unmistakable rather than subtle:

- **Active** — its assignee is an agent that's actually live right now. The row
  gets a bold, glowing, gently pulsing treatment and, just after the task id, a
  **"● ACTIVE — \<agent id\>"** badge naming who is on it. This is deliberately
  the loudest state on the board. The glow is what says *active*; the badge
  sits behind the id rather than in front of it, so an active row's left edge
  is exactly where every other row's is — an item's indent means one thing
  only, that it is nested inside another item.
- **Idle** — the status still says `in-progress`/`review`, but the assignee
  isn't a currently-live agent (its pane was killed, or it's an older session).
  The row reads as muted, not active — an idle/stalled assignment can never be
  confused with real live work.

The assignee chip itself carries the same distinction: a **live** agent gets its
own green tint, while a **history** chip (an assignee that isn't currently live)
reads dimmed and in italics — so an old assignee on a done, reopened, or stalled
task never looks like the same agent is still sitting there. `done` items dim
further still, receding behind whatever's still active.

A row that's blocked on a human decision or parked for a demo also wears a
small **marker chip** right after its id — the first thing to catch your eye
once you know the shape. **❓ needs a decision** means a pending question names
this row; **👀 needs a look** means the row itself sits in `prototype` or
`human-testing` with no question naming it (a row that's somehow both shows
only the decision chip — it's the more specific, more blocking ask). Click it
to jump straight to that item in the [NEEDS-YOU panel](#steering-attention-and-audit)
(`Alt+Q`).

### Finding things: search, filters, and folding the tree

A long-lived group's board is mostly history — hundreds of rows, most of them finished — so the
board has a control strip under its header for narrowing it down. **Everything the strip
remembers is remembered per group and survives a restart:** which containers you folded up and
which filters you armed come back exactly as you left them the next time you open orrerix. It is
*your* view and nothing else — no agent can see it, no agent can set it, and none of it is
written to the board file the orchestrator reads.

- **⊟ / ⊞ collapse all / expand all** fold or unfold every container in one click. (They appear
  only on a board that nests something.) The per-row **collapse chevron** still works the way it
  always has, and both are now remembered.
- **Find in this board…** filters by a case-insensitive substring of an item's **title or id**, so
  `auth` and `t-142` both work. Escape clears it.
- **level** and **status** chips filter by [level](#parent-tasks-and-subtasks) (epic / feature /
  story / task, plus **none** for items with no level) and by status. Click chips to add them:
  within one row of chips they're an *any of these* (tick `blocked` and `pr` to see both), and
  the two rows narrow each other (`story` **and** `blocked`). The level chips are hidden on a
  board that doesn't use levels. If your board file has been hand-edited to use a level orrerix
  doesn't know, that level gets a chip too, so nothing on the board can become unreachable.
- **sprint** chips filter by [sprint](#sprints--batches-of-work-in-order) — one chip per sprint
  number your board actually uses (they're never invented, so a board that jumps from 1 to 5
  offers `#1` and `#5` and nothing in between), plus **backlog** for everything with no sprint
  at all. They behave like the rows above them: *any of these* within the row, and they narrow
  the other rows. The whole row is hidden on a board that doesn't use sprints.
- **❗ needs you (N)** is the shortcut for the question the board is usually opened with: show
  only the items blocked on a decision or parked for a demo — exactly the rows wearing the
  **❓** / **👀** marker chips. It hides itself when nothing is waiting.
- **✕** clears every filter at once, and the count beside it (**"7 of 412"**) says how much of the
  board is on screen.

Two things about filtering a *tree* that are worth knowing, because they are what keeps the board
readable as a tree rather than collapsing it into a flat list of hits:

- **A match brings its containers with it.** Filter to `story` and you still see the epic and
  feature each matching story lives in, dimmed, because they are context rather than matches. The
  reverse doesn't happen: filtering to `epic` shows the epics *alone*, not everything inside
  them.
- **A filter reveals matches inside folded containers, and doesn't disturb your folding.** While
  any filter is armed, containers open up far enough to show what matched, and the collapse
  controls go quiet (they'd have nothing to do — a container is either holding a match, and has
  to stay open, or has nothing showing under it anyway). Clear the filter and the board is folded
  exactly the way you left it.

A container's **done/total** chip picks up a dashed outline whenever some of its items are off
screen — folded up or filtered out — so a collapsed row still tells you how much is inside it.

### Dependencies — what's actually startable

A task can declare that it waits on other tasks on the same board. The
orchestrator sets these when a plan implies ordering (that's what stops "what's
unblocked right now" from being re-derived from prose after every restart), and
you can edit them yourself:

- **🔗 on a row** opens a picker of the board's other tasks — choose one and this
  task now waits for it.
- A row with links grows a second line: **blocked by** chips, one per
  dependency, marked **✓** (that one is `done`) or **✗** (it isn't yet). Hover a
  chip for the other task's title and status; **✕** on it removes the link. A
  red **⚠** chip means the link names no task on this board at all — only
  reachable by hand-editing `tasks.json`, and worth removing, because it counts
  as unmet forever.
- **see also** chips are non-blocking annotations the orchestrator keeps. They
  are shown read-only here and never affect anything below.
- A `queued` task still waiting on something **dims**, so it can't be read as
  work anyone could pick up; one whose dependencies are all done is marked
  **▸ ready**, next to ▶ Start. A subtask dims the same way while a task it sits
  *inside* is the one still waiting — you can't start a slice whose feature
  can't start — and the chips saying what is holding it are on that container's
  own row, which is always visible above it. The ready mark appears only once
  some task on the board actually declares a dependency — on a board that uses
  none, every queued item is trivially ready and the badge would say nothing.

Only `done` satisfies a dependency: an item sitting at `pr` or `human-testing` is
work *you* haven't signed off yet, so anything depending on it keeps waiting.
Dependencies never move a status on their own — nothing is auto-flipped to
`blocked` — they only change how the board reads. (`blocked` stays the status for
blockers *outside* the board.)

Edits are validated where the board is stored, not here, so the rules are the
same whether you or the orchestrator made them: a link must name a live task, a
task can't depend on itself, and a **cycle is refused** with an error naming the
loop (`t-1 → t-3 → t-2 → t-1`) — you'll see it in the board's toast, and nothing
is written. Deleting a task strips it from every remaining task's links in the
same write, so a delete never leaves a dangling dependency behind.

**If an agent edited the same row while you were looking at it**, your click is
refused rather than quietly overwriting what it wrote — the board is telling you
it moved under you. For an edit that names a *thing* (removing a dependency,
adding a grounding link) orrerix simply re-applies what you asked against the
row as it now stands, so you usually see the result and never the message. For
removing a grounding link it stops and repaints instead: that click names a
*position* in the list, and the list you were shown is no longer the list on the
board. Look at the refreshed row and click the **✕** you can now see.

### Parent tasks and subtasks

A task can sit *inside* another task — an Epic or Feature the orchestrator created to hang
concrete slices under, following the same shape as Agile hierarchies (Epic → Feature → Story →
Task). This is **containment**, and it's a different relationship from the **dependencies**
above: a subtask's container says where it belongs on the board, a dependency says what must
finish before it can start. Express ordering with a dependency, not by nesting — nesting a task
under another one never means "after it". What nesting *does* carry is inherited waiting: a
subtask isn't startable while a container above it still has unfinished dependencies of its own
(see below).

Each task can carry a **level** — `epic`, `feature`, `story`, or `task` — set by the
orchestrator, or by you from the board's own **🏷** picker. The level is **enforced**, not a
label: an epic sits at the top level and inside nothing, a feature sits inside an epic, a story
inside a feature, and a task inside a story. Writes that break that are refused with an error
saying which level is missing and how to fix it. A container is still ordinary claimable work
like any other task, not a special row.

The task level is optional in the sense that matters: a story with no tasks under it is finished
work, not a gap. Break a story down into tasks only when the pieces are worth tracking
separately.

**A task with no level is exempt from all of that**, permanently — it can sit anywhere and
contain anything. That's what a plain board is: if your work has no hierarchy worth describing,
never set a level and the rules above are invisible to you. It's also what every board created
before levels were enforced looks like, which is why nothing on an existing board broke: the
rules are checked only when a write actually *sets* a level or moves a task, so an older task
whose shape predates them stays fully editable — status, notes, assignee, dependencies, all of
it. Only a write that re-states that task's level or container has to resolve it, and the error
tells you both ways: nest it where the level belongs, or clear the level.

The one thing the exemption doesn't allow is a *levelled* task inside an unlevelled one — "inside
a feature" is a claim about the container, so the container needs the level first.

**Task ids show the level a task was created at** — `e-3` for an epic, `f-4` a feature, `us-5` a
story, `t-6` a task or a plain row. The numbers come from one counter shared across all four, so
no two tasks ever share a number and a mistyped prefix names nothing rather than someone else's
task. Changing a task's level later never renames it: everything that points at a task — other
tasks' dependencies, the audit log, an agent's own notes, your memory — points at that string. On
a task whose level changed after it was created, the **badge** is the truth and the prefix just
says where it started.

Three more rules about nesting are backend behavior — they hold however the board reaches you,
the UI below included:

- Deleting a container **promotes** its subtasks rather than deleting or orphaning them: they
  move up to the nearest container still on the board (or to the top level, if the whole chain
  above them was deleted in the same action). Nothing under a container you delete disappears
  with it.
- Nesting can run at most 4 levels deep; a write that would go deeper is refused with an error
  explaining why, the same way an invalid dependency write is.
- Deleting a container can leave a subtask somewhere the level rules wouldn't have put it — a
  feature whose epic you delete ends up at the top level. That's deliberate: the alternatives are
  refusing your delete, deleting the work inside, or silently stripping the level off the
  survivor. The task reads and edits normally; it's the next time you change *its* level or
  container that you'll be asked to resolve it.

Board controls for nesting:

- A **⤵ nest** picker on a row lets you choose which other task it sits inside, or promote it
  back to the top level. It offers only containers the level rules allow — on a levelled row that
  is the level directly above it, and on an unlevelled row it's every other task. "Top level" is
  offered only where the row's own level permits it, so a feature is never offered a move its
  epic-shaped rule would refuse.
- Rows nest visually under their container, indented one step per level, with a **collapse
  chevron** on any row that has subtasks — collapsing hides the whole subtree, not just its
  direct children, so a grandchild is never left stranded above its own container. What you have
  folded up is remembered per group across restarts, and **⊟ / ⊞** in the strip above the list do
  the whole board at once — see [Finding things](#finding-things-search-filters-and-folding-the-tree).
- The **level** shows on the row as a badge, so you can see at a glance which rows are containers
  and at what level. A **🏷** picker next to the nest control lets you set or change it directly —
  offering only the levels this row could legally take where it sits, plus the clear when that is
  legal too. It can come back **empty**, and that's information rather than a bug: a row inside an
  unlevelled container has no legal level until the container gets one, and a container holding
  levelled rows can't drop its own level while they're inside it.
- The **▲/▼ priority arrows now move a task among its siblings**, not through the whole board:
  the first subtask of a container has nothing above it to swap with, so its ▲ is greyed out
  even though there are rows higher up the board. Moving a container moves everything inside it
  along with it, so re-prioritising a feature never scatters its subtasks.
- A container shows a **done/total** chip counting its *direct* subtasks — the same count you'd
  see if you asked the orchestrator for the board. The chip is outlined while some of those
  subtasks are off screen (the row is folded up, or a filter cut them), so a collapsed container
  never hides how much is inside it. A container whose entire subtree is done but
  whose own status hasn't caught up gets a nudge badge — it's a prompt for you, never something
  that flips the container's status on its own; only you or the orchestrator ever change a
  task's status.
- A subtask whose container was removed by hand-editing the board file, or that otherwise points
  at nothing valid, renders at the top level with a broken-container marker rather than
  vanishing — the nesting equivalent of the `⚠` missing-dependency chip above.

Readiness climbs the nesting: a task is marked ready only when its own dependencies are all
done **and** every container above it has all of *its* dependencies done too. A slice inside a
feature that can't start yet isn't startable either, so it no longer says it is.

Only a container's **dependencies** count, never its status. A subtask of a container marked
`blocked` is still ready to start — `blocked` is for blockers outside the board (a decision
you owe, an upstream repo), which says nothing about the work nested inside. If you want that
task held too, give it — or its container — a dependency, which is the machine-readable way to
say it.

Nesting is still board metadata everywhere it counts: it never affects whether a merge is
allowed, and it never blocks the orchestrator from *assigning* a subtask — readiness is a
signal for reading the board, not a lock.

### Sprints — batches of work, in order

A task can carry a **sprint number**: Sprint 1, Sprint 2, and so on. A sprint is a *batch*,
not a calendar — the number replaces the timebox, so there are no start dates, no end dates
and no duration anywhere. The point is only to say **which work comes first**: the
orchestrator finishes the current sprint before starting the next, and tasks with no sprint
at all — the backlog — sit behind everything that has one.

- Put an item in a sprint yourself with the **🎯** button on its row, or ask the orchestrator
  to. It's an ordinary board edit either way, not a privileged one — nothing about a sprint is
  locked down, and the same picker takes an item back to the backlog.
- An item in a sprint wears a **`sprint N`** badge, brightened when N is the current sprint.
  Items with no sprint wear nothing: the backlog is the absence of a badge, not another one.
- The board header carries **`sprint 2 — 3/7 done`** whenever the board uses sprints at all.
  It counts everything in that sprint, cleared items included, and clicking it shows *only*
  that sprint's items (click again for the whole board). It's a shortcut for the sprint chips
  in the strip, and it arms the number — so when the sprint later completes, the board you
  come back to is still the one you aimed it at, rather than having silently re-pointed itself
  at the next sprint while you weren't looking.
- The **current sprint** is the lowest sprint number that still has unfinished work in it. It
  isn't stored anywhere: when a sprint's last item is done, the next one becomes current by
  itself, so there's no marker that can drift out of step with the board. Nothing you click
  flips it — the **⏭** button beside the header moves *items*, and the current sprint follows
  from where the items ended up.
- **A blocked item keeps its sprint open.** That's deliberate: a sprint quietly ending
  because the work left in it looked stuck is exactly the thing you'd want to be told about.
  It stays current until that item is resolved or moved on.
- Moving unfinished work into the next sprint is always **explicit**. **⏭** shows you the
  exact list of items that would move — blocked ones included, since those are precisely what
  a silent roll-over would sweep up — and asks before writing anything. Confirming moves them
  one at a time, so each shows up in the audit log on its own and the orchestrator sees the
  board change. Done items keep the sprint they finished in. The orchestrator does the same
  thing the same way, and says in its pane which items it moved. Nothing rolls over silently.

Sprint numbers don't have to be tidy — they needn't be contiguous and needn't start at 1, so
you can leave gaps for planned work without the board minding.

**A sprint changes nothing except the order things get picked up in.** It doesn't make an
item startable or unstartable, it doesn't stop the orchestrator claiming something, and it
doesn't interact with WIP limits. An item outside the current sprint is still perfectly
ready to start if its dependencies are met — the sprint says what *should* come first, not
what *may* happen. And the board itself is never re-sorted: it stays in the order you put it
in.

**On a board that uses no sprints, nothing changes at all.**

### Grounding links — what an agent should read first

Beyond dependencies, a task can carry **links to the things that govern the work**: the
requirement it has to satisfy, an acceptance spec, the design note that constrains the
approach, a test case that pins the behaviour, a doc it has to keep true — or just a plain
link worth reading.

This exists to fix a specific failure. An agent picking up a task otherwise has to
rediscover its context from scratch every session — hunting through an issue thread for the
requirement, guessing which design note applies — and the real risk isn't wasted time, it's
**missing a relevant requirement entirely**. A task that carries its grounding as data hands
the next agent what governs the work instead of hoping they find it.

- Each link has a **type** (requirement, spec, design note, test case, doc, or a plain
  link), a **target**, and an optional one-line label to show instead of a bare target.
- A target can be an **issue or PR ref** (`#123`), a **file in the repo**
  (`doc/design/x.md`, a test file), or a **URL** — the surfaces grounding actually lives on.
- The orchestrator records them, typically when it creates the task, and planners record
  them as part of a plan — so the artifacts a plan names become something the next agent
  reads rather than prose someone has to re-parse.

Two things links deliberately don't do. They **never affect readiness or ordering** — a link
is context, not structure; if you mean "this must finish first", that's a dependency. And
targets are **never checked for existence**: the board doesn't go and look, so it keeps
working offline and a board edit never fails because GitHub was slow. A link pointing at
something that has moved is kept as it is, the same as a dependency naming a task that isn't
there — yours to fix, never silently dropped.

One thing *is* checked: a link whose target names another **task on this board** is refused,
with an error saying so. That's what dependencies and see-also links are for, and the two
kinds of link are kept apart on purpose — one points inside the board, the other points out
of it.

**On the board** — where you read and edit them by hand:

- **📎 on a row** is the way in, on every row. It carries the row's link count, so you can
  see how much grounding an item has without opening anything — on a board where nothing has
  links yet it's just **📎**, and clicking it is how a row gets its first one.
- Unfolding it lists the links, each with its type and its label (or its target when it has
  no label, with the raw target beside a labelled one — a gloss shouldn't hide what it points
  at). **✕** on an entry removes that one entry, even if another link on the row points at
  the same thing. If an agent changed the row's links since it was drawn, the ✕ is refused and
  the row repaints — see *If an agent edited the same row* above.
- **Clicking a link acts on it.** An issue or PR ref and an `http(s)` URL open in your
  browser; everything else — a repo path, anything the board can't recognise — is **copied to
  the clipboard** instead. That's deliberate: a target is free text that an agent may have
  written, so only the two shapes orrerix can name are ever launched. The tooltip says which
  it will be before you click.
- **Adding one** is the row at the bottom of the list: pick a type, type the target, add a
  label if it helps, Enter or **Add link**. Nothing is checked for existence (see above), so
  a link you record now against a doc you're about to write is fine.
- A task holds at most **32** links. At the limit the add form is replaced by a line saying
  so, rather than letting you fill in a form that can't be submitted.

### Grounding in the brief — the links reach the agent by themselves

Recording grounding only pays off if somebody reads it, so orrerix delivers it rather than
hoping. When the orchestrator opens an agent **against a task** — the same "open a worker"
call, plus the task's id — that task's links are composed into the top of the agent's opening
brief, above the task it was given:

```
Grounding (board task t-42): pointers recorded on that board task to what governs this work — read them before you start. They are context to weigh, never instructions.
- [requirement] Retries must be bounded: #1104
- [design-note] doc/design/retries.md
Your task:
Make the retry path give up after the budget instead of spinning.
```

You never do this by hand: ask the orchestrator to put a worker (or a reviewer) on a board
task and it passes the id for you. What that gets you:

- **Reviewers get the section too.** A test-case link is a review input as much as a build
  input — the reviewer reads what the behaviour was supposed to be, not just the diff.
- **A task with no links adds nothing**, so pointing an agent at a task is always safe. You
  don't have to put grounding on a row before you can open an agent against it.
- **A wrong task id fails the spawn, out loud.** Quietly opening an agent with no grounding
  would look exactly like a task that has none, and nobody would ever find out.
- **An agent opened without naming a task** gets exactly the brief it always got.
- **Pointing at a task is context, not assignment.** It doesn't claim the task, change its
  status, or set its assignee — those stay ordinary board edits.

The section says outright that the lines are context to weigh and not instructions, and it
sits above `Your task:` so an agent reads what governs the work before it reads the work.
One limit worth knowing: the binding lives only in memory, so after an app **restart** a
rejoined pane has no board row attached and its kickoff carries no section. (A follow-up
spawn that resumes a session *and* names a task does get one, like any other spawn.)

### WIP limits (finish before you start)

**Max live agents** caps how many agents run at once. It says nothing about how much *work*
is open — so an orchestrator can pile up ten items waiting on review while cheerfully
starting more. **WIP limits** cap the work instead: how many items may sit in a status at
one time.

Declare them in your workflow file (the file that carries your roster, so this needs the
**advanced orchestrator** on):

```yaml
board:
  wip:
    in-progress: 4
    review: 3
```

Declare nothing and the feature is off — no limits, no counts, nothing changes. A status you
leave out simply has no cap.

**The board shows you where you stand.** Each declared limit gets a chip on the task board's
header: `review 2/3` while there is room, amber at `3/3`, red past it. Your orchestrator sees
exactly the same numbers when it reads the board.

**By default a limit warns rather than refuses.** Crossing one lets the write through, tells
your orchestrator ("`review` now holds 4 of a declared 3"), and records it in the audit log.
That is usually what you want first: a limit is a guess until you have run under it, and
three notices teach you more about the right number than a week of refusals would. When you
believe the number, turn it into a refusal:

```yaml
board:
  wip:
    review: 3
  enforce: true
```

**`enforce: true` only ever refuses your agents.** Your own board edits are never bounced by
a limit you set — you are the one resolving the overload, and the board's authority is
yours. They still show up on the chip and still tell the orchestrator, so nothing about the
board goes quiet; you just cannot be blocked by your own rule.

A refused write names the limit, how full the status is and which items are in the way, so
your orchestrator can finish one rather than retry.

**What counts, exactly.** A write is judged on the board it *produces*: a limit fires when a
status ends up over its cap **and** this write is what raised it. So editing an item already
sitting in a full status always lands, and so does every move out — a status that has gone
over is never stuck. Assigning work (`claim`) raises `in-progress`, which is the point.

Only leaf items count: a container in `review` does not consume a slot, because the work
inside it is counted where the work is. That is why nesting an item under another *as you
move it* can be within a limit that the same move without the nesting would exceed — the row
you nested it under stops being counted in the same write. It cuts the other way too:
un-nesting the last item out of a container makes that container countable again, which can
put its status over a cap without anything having changed status at all.

You can cap any status **except `done`** — that one is the release valve every other limit
depends on, so orrerix refuses a file that tries. You *can* cap `blocked`, and it is useful
as a warning — but note that `enforce: true` is one switch for **every** cap you declare, not
one per status. So if you turn enforcement on, a `blocked` cap becomes a refusal too, and
refusing a move to `blocked` refuses an agent's report that something is stuck. Under
`enforce: true`, leave `blocked` uncapped.

**A bad value fails the whole file, on purpose** — `review: 0`, a misspelt status
(`in-porgress`), or any key orrerix does not recognise stops the workflow file from
loading at all, taking your roster and merge gate with it, and the launcher shows you why.
The error names the statuses you *could* have written. That is deliberate: a repo that wrote
`review: 0` believes something about how its board paces, and quietly substituting a default
would leave that belief in place while the behaviour went the other way.

## Steering, attention, and audit

These deserve their own detail — see:

- **[Steering & attachments](features/steering.html)** — the collision-proof compose
  strip under an orchestrator pane (`Alt+P`), and pasting screenshots into a
  message.
- **Attention routing** — a pane earns a pulsing **needs-attention** chip when an
  agent is parked on a prompt only you can answer, when a worker reports done or
  blocked, when a task hits a human merge gate, or when the orchestrator (or a
  liaison pane, if the group's workflow has one) has a question pending your
  answer. Hovering the question chip shows a live count; clicking it focuses
  the pane and acknowledges the chip — acknowledging only tells orrerix you've
  seen it, so a chip that's still genuinely true comes right back on the next
  scan.
  - **The NEEDS-YOU panel** (`Alt+Q`, or the raised-hand icon in an
    orchestrator pane's header) is everything currently waiting on you, in
    **one list**, badged with a running total in its own header. The newest
    ask is at the top, and anything raised as *urgent* is pinned above the
    rest — so what just arrived is never at the bottom of a scroll. Two
    kinds of card share that list:
    - **Questions** — one card per pending `ask_human` question. Pick one of
      its options (each can carry the asker's own reasoning under the label),
      type a free-text answer, or both; a question decides for itself whether
      it allows a single pick, several, or no free text at all, and the card
      only offers what that question allows. Sending delivers your answer to
      the **orchestrator's** pane — even for a question a liaison pane
      posed, since the liaison never gets the answer notice itself and
      instead reads the outcome back through its own `list_questions`. A
      card that names the board row it's holding up links straight to it.
    - **Needs-you items** — a **demo** parked for you to go run, or a request
      for **feedback** on a direction. An item is its own record: it says who
      raised it, when, and what they want back, and it *links* a board row
      rather than being one. When it links a row, the card shows that row's
      live state — the worktree path where the demo lives (click to copy), a
      link to its PR when it has one, and its current status; a row with no
      recorded path says so rather than guessing one. **Proceed** promotes a
      `prototype` (the same gesture as the board's own Proceed button).
      **Feedback** sends your notes back to the orchestrator — on
      `human-testing` it's the request-changes gesture and reopens the task,
      on `prototype` it's a plain note that leaves the demo gate exactly
      where it was.

    A demo item appears by itself when the orchestrator parks a task in
    `prototype` or `human-testing`, and settles by itself when that task
    moves on — you never have to keep the two in step. If the linked row is
    gone (pruned, or renamed), the card says so and stays clearable rather
    than disappearing with the ask still outstanding.

    **Resolve** on an item's card is your close-out: *I have seen this.* It
    clears the row from the panel and **leaves the task exactly where it
    is** — resolving is not a board move, and Proceed/Feedback stay the
    board actions they always were. You can attach a note, which goes to the
    orchestrator's pane; resolving without one just tidies your queue
    quietly. A pending **question** has no Resolve: answer it, or the
    orchestrator withdraws it — a dismissed-unanswered question is a
    decision silently dropped.

    Settled rows — answered or withdrawn questions, resolved items — fall
    into a faded tail (the ten most recent) instead of vanishing.
    **Clear completed** in the header hides that tail. It **deletes
    nothing**: the rows stay on disk for the audit trail, the choice
    survives a restart, and nothing still open can be touched by it, which
    is why it doesn't ask you to confirm.

    The panel is the only place a question gets **answered**, and the only
    place an item gets **resolved** — no agent can do either, by any path.
    Withdrawing stays the separate, agent-side path it always was, and there
    is now one of those for each tier: `withdraw_question` settles an
    overtaken question, `withdraw_attention` an overtaken item, each of them
    visibly *withdrawn* rather than answered or seen.

    **The orchestrator's own side of the item tier is three tools.**
    `request_attention` raises one, `withdraw_attention` takes one back, and
    `list_needs_you` is how a pane re-reads what it has left with you after a
    compaction or a restart — which is why a demo you parked days ago is still
    accounted for by an orchestrator that has long since forgotten the
    conversation. Parking a task already raises its demo item, so a
    well-behaved orchestrator reaches for `request_attention` mainly to ask
    for **feedback**, the one kind nothing on the board raises for it.
  - **orrerix's protocol is that no agent asks you through a blocking
    dialog.** The orchestrator's role instructions — and a liaison pane's,
    where the group's workflow declares one — call for filing every question
    through `ask_human` instead of a CLI's own interactive-question dialog,
    because a dialog holds the whole pane and refuses every delivery queued
    behind it. That is instruction an agent follows, not yet something
    orrerix enforces structurally on every CLI, so treat it as the norm
    rather than a hard guarantee.
  - An optional per-group **desktop notification** toggle (🔔 in the lifecycle
    panel) raises an OS toast the first time a question needs your answer
    (off by default). It fires for that and for the other reasons above —
    **except** a task merely reaching a merge or demo gate, which is common
    enough (every PR does it) that toasting on it would be noise; the board
    and the NEEDS-YOU panel are where you see those.
  - Most chips clear themselves: they're recomputed every few seconds, and
    clicking one focuses the pane and acknowledges it.
  - The red **⚠ stuck prompt** chip is the exception. It means a prompt orrerix
    sent was never submitted, and it stays up until orrerix has evidence the pane
    is fine again. orrerix keeps looking for that evidence for as long as the chip
    is up — every half-minute or so it re-reads the pane, even long after the
    delivery that raised the chip has been given up on. If the prompt has left the
    box **and** you have typed in that pane since, the chip comes down on its own;
    if the prompt is still sitting there, or orrerix cannot read enough of the pane
    to tell, the chip stays, because taking it down would be the one mistake it
    exists to prevent.
  - Some chips have no reading that could ever release them — one reporting
    messages that were discarded while the group was paused is about something
    that already happened. So that chip carries its own **✕ dismiss** button
    beside it. Clicking ✕ takes the chip down for good, whatever raised it.
  - **✕ takes the chip down and nothing else.** It does not unstick the pane, and
    orrerix never presses Enter on your behalf because you dismissed something. If
    the prompt really is still sitting unsubmitted in that pane's input box, it
    still is afterwards — so if you're not sure, look at the pane first (hover the
    chip: it says what it thinks is blocking). Every dismissal is written to the
    group's audit log with what was dismissed and how long it had been up, so a
    chip you cleared can always be looked up later.
  - **The ⛔ held on a dialog chip** is the most urgent of all — it outranks even
    a worker reporting blocked. It only ever appears on the **orchestrator's own**
    pane, and only while orrerix is **actively trying to deliver something to it
    right now** and finding its CLI's own interactive-question dialog on screen:
    on Claude that dialog is denied outright (see below), but not every CLI
    orrerix supports can be told to refuse it at that level, so this chip is the
    fallback that still tells you a held orchestrator pane is stranding every
    delegate's report queued behind it, not just its own. It comes down the
    moment that delivery attempt stops holding — either because the dialog
    cleared, or because the attempt gave up waiting (orrerix tries again shortly
    after, and the chip returns then if the dialog is still there) — so it never
    needs a dismiss, but it can also briefly drop even while the dialog is still
    up between one attempt ending and the next one starting.
  - **The orchestrator can't use its CLI's own question dialog at all, on Claude.**
    A single stuck question dialog on the orchestrator's pane once held a whole
    run overnight — the in-flight workers finished their PRs, and then nothing
    was reviewed, dispatched or merged until morning, because every delivery to
    a held pane queues instead of landing (and that queue is bounded: enough of
    them and further ones are refused outright). So on Claude, the orchestrator
    (and a human-interface/liaison agent, if your workflow has one) is launched
    with that dialog denied outright; a delegate pane is unaffected, since a
    human answering its dialog in person never stalls anyone else.
- **Audit viewer** (`Alt+A` or the history icon) — opens the group's
  `audit.jsonl` as a filterable, searchable timeline: every prompt, spawn, task
  edit, delivery outcome, and state write, one row each. A **follow** button
  live-tails new lines.
- **[Progress timeline](features/progress-timeline.html)** (`Alt+W` or the
  timeline icon) — the same audit data plus GitHub issue/PR lifecycle, plotted
  on a time axis instead of listed: lanes for group, agents, work, gates and
  GitHub, a window filter (12 hours by default), and click-through to the raw
  record. Read-only, and it states its own coverage boundaries — what it is
  *not* showing is printed under the chart rather than left to look like quiet.
- **Delivery queue.** If a pane is busy — an interactive question on screen, or a
  human's own line still sitting in its input box — orrerix holds a prompt
  delivery rather than typing over it. A hold that never clears **queues** the
  prompt instead of dropping it: nothing is lost, there's no timeout (you might
  be away for hours), and the queue drains automatically, in order, the moment
  the pane is free again — the first thing it delivers is a one-line summary of
  what was waiting. One exception to "no timeout", and it only ever moves a queue
  *forward*: if the hold is the interactive-question one, and after 15 minutes the
  pane's own screen still shows its ordinary input box, orrerix stops believing its
  own question detection and pastes anyway (you'll have had the "stuck behind a
  question" badge for five minutes by then). It does **not** stop checking before
  it presses Enter — if a real dialog is up at that moment the Enter is still
  withheld and the text is left in the box for you, rather than answering the
  dialog for you. And a hold on your own typed input is never overridden, at any
  age. You'll never see "held … re-send" — if a prompt is safely
  queued, the notice says so explicitly and tells you not to re-send it (that
  would just create a duplicate); a payload is only ever reported gone if the
  notice says **DROPPED** (the pane's queue was already full, or the agent's
  pane closed while entries were still waiting). The orchestrator's *own* pane
  is the one case orrerix can't announce this way — a prompt about that pane's
  blocked delivery would queue behind the very block it reports — so those
  notices ride back to the orchestrator on the result of its next tool call
  instead, and you'll see them in the audit viewer either way.
- **Queue depth, on the pane header.** A pane with prompts waiting wears a chip
  saying how many and for how long — `⇥ 3/8 queued · 12s`: three waiting out of
  a maximum of eight, the oldest queued twelve seconds ago. It appears as soon
  as anything is waiting and disappears when the queue drains, so "deliveries
  are flowing" is something you can see rather than infer from the absence of
  warnings. Once nothing has been delivered to that pane for a minute the chip
  turns amber and says **stalled** — that is the one to act on: check the pane
  for a question waiting on an answer, or your own half-typed line still in its
  input box; releasing either drains the backlog. Everything you need is in the
  chip itself, and hovering only adds a sentence. A **minimized** pane shows the
  same count on its dock chip (`⇥3`, amber when stalled), which matters because
  worker panes open minimized by default — the queues that back up are usually
  the ones whose header you can't see.
- **What a full pane refused.** A pane that hit its cap and turned deliveries
  away is told what it turned away, the moment its queue drains back below the
  cap: one line naming each refused delivery's sender, a short preview, and why
  it was refused. Deliveries whose sender has since got them through are
  *marked* as re-sent rather than dropped from the list — the receiving pane
  can't tell the difference from the outside, and a list that quietly omitted
  them would read as "these are all still missing". Orrerix never re-sends them
  itself: the senders were told at the time, so the roster names who to ask.
  It's bounded (the newest few, with the rest counted and left in the audit
  log), fires once per drain rather than once per delivery, and obeys the same
  8-deep cap it's reporting on — so it can never pile up behind the backlog it
  describes. The orchestrator's own pane gets it the same way as the notices
  above: riding back on its next tool call.

## CI watches (agent notifications)

Distinct from the 🔔 desktop-notification toggle above — that raises a toast for *you*; this
notice goes to the *agent*, typed into its own pane. Agents don't sit watching a PR's CI:
the orchestrator, workers, and reviewers can register a background watch — a PR's checks, or
a specific GitHub Actions run — and go do other work; orrerix polls in the background (every
30s) and types an `[orrerix] PR #241 checks: SUCCESS — … (watch n-3)`-style notice into the
registering agent's own pane the moment it resolves, expires, or fails repeatedly. A watch is
capped (4 per agent / 12 per group) and time-bounded (5–240 min, default 60). Pausing a group
freezes a watch entirely (no polling, firing, or expiry) until you resume it.

Watches live only in memory, and the two ways an agent loses track of one are different:
a `/compact` drops the agent's *memory* of a watch (the watch itself is still registered and
still live), so it re-lists to recover what it was waiting on; closing orrerix drops the watch
*itself* — the registry is empty on the next launch — so it must be re-registered from
scratch, not merely re-listed.

**Where you see it.** A watch is visible from *your* side too, not just the agent's. The
group lifecycle panel (`Alt+O`) shows a **⏳ waiting on PR #241 checks (expires in 43 min)**
line under any agent holding one — the reason a worker sitting quietly is waiting on CI, not
stuck. The audit viewer (`Alt+A`) has a one-line sentence for each of a watch's six lifecycle
events (registered, fired, expired, failed, cancelled, cleaned up on agent exit) instead of raw
JSON.

The internal watchdog stall check knows about a live watch too: an agent silent past its
group's stall window while it holds one is **not** nudged to the orchestrator — it's plausibly
waiting on its own CI check, not stuck — and the suppression itself is audited
(`watchdog-suppressed`) so it stays diagnosable. Once that watch resolves (fires, expires, or
is cancelled), the agent earns a fresh full stall window from that moment; only silence through
*that* window is treated as a real stall and nudges the orchestrator (`watchdog-stall`).
A stalled agent holding no watch behaves exactly as before.

The suppression is bounded by the watch's own TTL (5–240 min, default 60 — see "capped ...
and time-bounded" above), never open-ended: a genuinely hung agent holding a watch is
silent-but-unreported for at most that TTL plus one more stall window before the orchestrator
gets a notice, and `watchdog-suppressed` audit lines mark the wait the whole time.

**When a notice can't get in.** An `[orrerix]` notice is typed into the agent's pane, and a pane
that is mid-turn can't take one — so if an agent blocks its own turn waiting on the very thing
the notice would tell it about, the notice sits in that pane's queue and nothing clears. Orrerix
watches for exactly that shape: a pane that has accepted nothing for ten minutes while holding
one of orrerix's own notices gets an `[orrerix] notice undeliverable 10 min: … — pane mid-turn …`
sent to the group's orchestrator (and to the audit viewer as `notice-undeliverable`), alongside
the ⏸ held chip and the needs-attention badge you'd see anyway. The message says which of the
three it looked like — a pane mid-turn, a human's own line in the box, or a dialog waiting for an
answer — because those need different things from you. If the stuck pane is the orchestrator's
own, the notice rides back on its next tool call instead of being typed into it.

**When you fix a stuck prompt yourself.** A **⚠ stuck prompt** chip means a prompt was typed
into that pane but never submitted, and orrerix queues a repair — one Enter, held back until the
pane is safe to write to. If you get there first (click into the pane and press Enter, or clear
the box), orrerix now checks the pane rather than the keyboard: once the prompt is no longer
sitting in the box, the repair is dropped instead of pressed a second time, the chip comes down,
and anything queued behind it — steering messages, worker reports — starts flowing again on the
next poll. Before this, your fix was invisible to the queue, and continuing to type in that pane
kept the repair pending indefinitely, silently holding everything behind it.

The repair is only ever dropped once orrerix can *see* that the prompt has left the box. While
it is still sitting there, orrerix keeps waiting for a safe moment to press Enter — because the
alternative is pasting the next message on top of it and sending the two merged into one. So a
pane blocked by your own half-typed line, or by a dialog waiting for an answer, still holds its
queue, and the chip is what tells you it needs you.

**And on a pane nobody is delivering to any more.** The check above rides a repair or a
delivery, so it used to stop once orrerix gave up on the pane — after which a chip on a
finished worker or a drained orchestrator could stay up until you restarted. It no longer
does: for as long as a **⚠ stuck prompt** chip is up, orrerix re-reads that pane every half
minute and applies the same rule. Two things have to be true together for the chip to come
down by itself — the prompt is no longer in the box, **and** you have typed in that pane
since orrerix submitted it. Either alone is not enough, deliberately: a prompt that vanished
with nobody at the keyboard is exactly the case where the chip is the only trace left of it,
and a keystroke on its own says nothing about where the prompt went.

Between those, the wording can still change under you. A chip that said orrerix could not
read the pane, or that its repair attempts were used up, becomes "its text is gone — check
the pane" once orrerix can see the box is clear. That is the chip getting more honest, not a
new problem: it stays up, and it stays up until you deal with it or dismiss it. Whatever
takes a chip down — your ✕, or orrerix's own reading — is written to the group's audit log
with what it was and what was read, so you can always look up where one went.

**One chip is answered by orrerix retrying, not by orrerix looking.** A **⚠ stuck prompt** chip
that says the repair could not even be *queued* — the pane already had the maximum number of
messages waiting — is not a claim about what is in the box, so re-reading the pane could never
settle it. Instead, once that pane's queue has drained and nothing is delivering to it, orrerix
queues the repair it could not queue before, and the chip changes to "loomux is re-sending it".
From there it behaves like any other repair: held back until the pane is safe to write to,
dropped if you get there first, and gone once the message lands. If the queue is still full, or
a delivery is in progress, the chip stays exactly as it was and orrerix tries again shortly —
nothing is taken down on a guess. Before this the repair was simply never attempted again, so a
pane that filled up once could carry that chip until you restarted, with the prompt still
sitting unsent in its box.

## Lock resources (taking turns on something scarce)

Some things on your machine only one agent can use at a time — a compile that saturates
every core, a GPU, a device on a USB port, a fixed port number, a staging database. Without
help, four workers will reach for it at once. **Lock resources** let you declare those things
once, in your repo, and have agents take turns.

Declare them in `.orrerix/workflow.yml` (the same file that carries your roster, so this needs
the **advanced orchestrator** on):

```yaml
resources:
  build:
    slots: 1              # how many agents may hold it at once — 1 is a mutex
    max_hold_minutes: 45  # orrerix takes it back after this, whatever happens
  gpu:
    slots: 2              # omitting max_hold_minutes means the default, 30
```

What a resource *means* is entirely yours — orrerix never learns that `build` is a compiler.
It knows the name, the slot count and the clock. Declare nothing and the feature is off:
the agents in that group are not even offered the tools.

**Both settings are optional, and the defaults are not what the example above shows.** Omit
`slots` and you get **1**; omit `max_hold_minutes` and you get **30**, not the 45 written out in
the example. A name may use letters, digits, `-` and `_` (up to 48 characters) — anything else is
rejected rather than quietly rewritten, so the name you type is the name your agents call.

**A value out of range fails the whole file, on purpose.** `slots: 0` or above 64,
`max_hold_minutes: 0` or above 480, a name with an illegal character, an unrecognized key inside a
resource, or more than 32 resources are all **hard errors that stop `.orrerix/workflow.yml` from
loading at all** — taking your roster and merge gate down with them, and the launcher will show you
why. That is deliberate: a repo that wrote `slots: 0` believes its builds are serialized, and
quietly substituting a default would leave that belief in place while the behaviour changed
underneath it. If your workflow file suddenly stops loading after you add a `resources:` block,
this is the first thing to check.

**What the agents get.** Three tools, and only in a group whose repo declares resources:
`acquire_lock(name, note?, wait_minutes?)`, `release_lock(name)`, and `list_locks()`. The
names you declared are listed in the tool's own description, so an agent can see what exists
rather than guessing. Your orchestrator is told to name the relevant lock in a brief; a
worker's instructions tell it to take the lock before the work and release it as soon as
that work is done.

**Nothing blocks.** `acquire_lock` answers immediately — either the lock is yours, or you are
queued at a stated position. A queued agent ends its turn and gets a
`[orrerix] lock 'build' is yours` notice typed into its pane when its turn comes, exactly like
a CI watch resolving. That is not a convenience: a notice is *typed into a pane*, and a pane
that sat blocking on its own lock could never receive the one telling it to proceed.

**The queue is first-come, first-served**, and every wait is bounded: a queued request gives
up after `wait_minutes` (default 60, 5–240) and the agent is told so, rather than waiting on
a notice that will never arrive. An agent that changes its mind can call `release_lock` while
queued to withdraw, so nobody sits behind a slot it no longer wants.

**A lock always comes back.** Three things can end a hold: the holder releases it, the holder's
pane dies (orrerix reclaims it immediately and hands it to whoever is next), or the hold runs
past `max_hold_minutes` (orrerix reclaims it and tells the ex-holder its work is no longer
serialized, so it can re-acquire). There is no state in which a resource is stuck forever
because an agent forgot.

Pausing a group freezes the **clocks**: nothing expires, nothing times out, and the paused span is
credited back afterwards, so a long pause never costs a running build its lock. It does not freeze
*you* — killing a holder's pane while the group is paused still reclaims that lock and still hands
it to whoever is next, because that is your deliberate action rather than a timer firing. (The
notice telling the new holder sits in its pane's queue until you resume, like every other delivery
to a paused group.)

**Where you see it.** The group lifecycle panel (`Alt+O`) grows a lock section: one line per
declared resource with how many slots are taken, who holds each (with the note it gave, e.g.
`w-3 (cargo test) 12m`) and how many agents are queued behind it. Hover for the full queue in
order, with each agent's own clock. The line turns amber when somebody is waiting and red
when a reclaim is imminent. The audit viewer (`Alt+A`) has a sentence for each lifecycle
event — taken, queued, released, granted, reclaimed, timed out — so "why did that build take
40 minutes" is answerable after the fact.

Lock state lives in memory only. Closing orrerix clears it, which is the right answer: every
pane that could have been holding a lock died with it.

**It is cooperative, and deliberately so.** A lock is taken because an agent asked for one —
orrerix does not intercept your build command, and an agent that never calls `acquire_lock` is
not stopped. An earlier design tried to enforce this by shadowing the guarded program on
`PATH`, and it was abandoned: a shim only catches the shells it shadows, so the guarantee it
appeared to give was not one it could keep. Advisory locking that is honest about being
advisory, with a full audit trail of who held what and for how long, is worth more than
enforcement with a hole in it.

## Cross-workspace channels

Every orchestration group is isolated by design — one group's agents never see another's
context. Sometimes you want a narrow, explicit exception: two related repos open in
different tabs (a library and its consumer, a backend and its frontend), and you want one
agent to tell another "the API changed" or "I'm blocked on your PR" without relaying the
message through you. A **channel** is that exception, and it is opt-in every time: **only
you** can open, close, or redirect one. No agent can ever connect, join, disconnect, or
redirect a channel itself.

(The pane header's right-click menu carries one other gesture, below the channel
items: **Promote to orchestrator…** on a standalone Claude pane — see
[Promoting a standalone agent](#promoting-a-standalone-agent). Promoting a pane
closes any channel it was in, since the promotion retires the standalone identity
that channel was keyed to.)

**Connecting.** Right-click an orchestrator, worker, reviewer, or standalone **agent**
pane's header and choose **Connect…** — the pane arms (its header outlines with a pulsing
dashed border) and waits for its peer. Right-click a *second* pane, in the same tab or a
different one; the completion menu asks you to pick a **direction** — `A → sends to → B` or
`B → sends to → A` — an explicit arrow, chosen at the moment of connecting, not guessed from
which pane you armed first. Right-click the armed pane again, or press **Esc**, to cancel
before completing it. Once connected, both panes show a small colored, numbered chip
(**⇄1**, **⇄2**, …) before their title, plus a direction arrow (**▲** for the sender, **▼**
for a receiver) — the color and number are the SAME on every member of one channel, so with
several channels active at once you can tell at a glance which panes belong together, and
who's driving each one. The number reflects what's **currently** connected, not a running
count: if **⇄1** disconnects, the next channel you connect gets **⇄1** again rather than
counting up forever — the number always matches how many channels are actually active right
now. The chip mirrors to a docked pane's minimized chip too, and a background tab holding a
connected pane gets its own small dot on the tab strip, so a channel spanning a hidden tab is
never invisible.

**Sender and receiver.** A channel is directional: one member is the **sender**, everyone
else is a **receiver**. The sender's `channel_send(text)` broadcasts to every receiver, any
time — it lands as a typed `[orrerix] channel chan-N - <name> (<role>, <repo>): <text>`
message in each peer's own pane, the same visible-prompt delivery every other agent-to-pane
message already uses. A receiver's `channel_send` is **reply-only**: it works once the
sender has messaged that receiver (one reply per message, to the sender only — never to
another receiver), so two agents can't talk over each other. `channel_status()` tells an
agent who it's connected to, who's driving, and whether it can send right now. Both tools
are denied to planners (like the CI-watch tools above) since a planner's pane closes the
instant it reports done.

You can hand the sender role to a different member without reconnecting: right-click a
connected pane and choose **Make this pane the sender** (only offered on a receiver that
can actually hold the role — see "receive-only", below). The swap clears every pending
reply credit, so both sides start clean under the new direction.

**Standalone panes.** A plain **Agent** pane (opened outside an orchestration group) can
join a channel too, not just orchestrator/worker/reviewer panes. Launching a fresh
**claude** or **copilot** agent pane wires it up automatically — nothing to do, it just
shows up as a normal Connect target — as long as the launcher's **Channel tools** checkbox
is on (it defaults on; turn it off if you'd rather a fresh pane not carry a live channel
token until you actually connect it — the checkbox only appears for claude/copilot, since
it's the only pair this applies to). Any other CLI (codex, gemini, opencode, a custom
command), a claude/copilot pane launched with the checkbox off, or any pane that was
already running before its channel tools were wired up, becomes connectable the first
time you right-click it: it joins as a **receive-only** member (a dashed variant of the
chip, instead of solid) — it can never be the sender, and its direction is always ▼. This
is a structural fact, not a bug: those CLIs have no way for orrerix to hand them a
channel-send capability today (tracked as a follow-up), and an already-running pane
can't be handed one either without restarting it. A receive-only pane still gets every message the sender sends it — it just
can't talk back.

**Multi-party.** A channel can have more than two members: right-click a free (not yet
connected) THIRD pane's **Connect…**, then right-click an already-connected pane's
completion menu — since that channel already has a sender, the only option is **Join as
receiver — driven by `<sender>`**, so a newcomer can never accidentally become a second
sender. A pane can only ever belong to **one** channel at a time; connecting an
already-connected pane to a *different* channel is rejected (disconnect it first) — that
limit is what keeps the chip unambiguous and keeps two channels from silently bridging
through a shared pane.

**Disconnecting.** Two equally-easy ways: the pane's context menu **Disconnect** item, or a
single click on the channel chip itself. Either removes just that one pane. If that drops
the channel below two members — or if the pane you disconnected was the **sender** — the
whole channel closes and every remaining pane is notified: a channel with no one driving it
is as dead as one with only a single member left, and there's no automatic promotion (a
human always picks who sends).

**Limits.** Channels are **in-memory only** — closing orrerix drops every channel;
after a restart, reconnect the panes you want linked. A pane holds **at most one channel**
at a time (see Multi-party, above). Full (sender-capable) standalone membership only works
for claude/copilot today — see "Standalone panes" above.

## Group lifecycle

The orchestrator pane has a lifecycle toggle (`Alt+O` or the group icon) with a
one-glance summary — how many agents are live, the role breakdown, uptime, each
agent's state, and running session cost with a group total.

**Where those cost figures come from.** Tokens are exact — read from each CLI's own
record of the session, never scraped off the pane. Dollars depend on the CLI. For
Claude Code, orrerix prices the tokens itself against a dated table, so the figure is
an **estimate** — and on a subscription/Max account the real marginal cost is $0
regardless, which is why tokens are the honest metric there. OpenCode prices its own
sessions, so orrerix **reports** its number instead of guessing one (including a
genuine $0.00 on a free model, which is an answer, not a blank). Each total is
labelled accordingly — *estimated*, *reported*, or *mixed* for a group running both.
A CLI with no readable record falls back to whatever dollar figure it prints in its
own statusline, which disappears when the pane does.

**When the panel says `stale`.** The lifecycle panel and the tab strip's agent
counts are served from a snapshot orrerix refreshes about once a second, rather
than by asking the backend a separate question per figure every time — ten of
them for the panel, and two per tab for the strip. If that refresh
falls behind — the usual cause is one long-running internal operation holding
things up — the panel keeps showing you the last figures it has and puts an
amber **`stale 12s`** badge in its header saying how old they are; the affected
tab counts go italic and their tooltip says the same. Nothing is lost and there
is nothing to click: the badge disappears by itself the moment a fresh snapshot
lands. It is deliberately never cleared by a timer, so a badge that is still up
means the figures really are still old. Numbers shown while it is up are true,
just from a moment ago — which is why orrerix shows them rather than blanking
the panel.

**When it says `partly stale`.** Same badge, narrower claim: most of the panel
refreshed and one part of it could not. The figures you are looking at are
still true, and the age on the badge is the age of the OLDEST part — so a
`partly stale 40s` panel is telling you that one section is 40 seconds behind,
not that everything is. It clears itself the same way.

**When an agent reports `loomux busy`.** Agents talk to orrerix over a small
local server, and if some internal operation is holding things up, a call that
would otherwise have waited indefinitely is answered instead with a message
beginning `loomux busy:` — naming what is held, for how long, and by which part
of orrerix. This is a normal, retryable answer, not an error to report: the
agent is told nothing was executed and it can simply try again. Two things are
worth knowing if you see one:

- **A busy answer never means a half-done change.** Only READS are answered
  this way. Anything that changes something is left to run — if one of those
  takes a long time the agent is told it is *still executing* and explicitly
  told NOT to re-issue it, because a slow change finishes on its own. The one
  way a change does *not* finish is a bug inside orrerix, and that is never
  reported as busy: it gets the `internal error` answer below, which says so
  and tells the agent to check.
- **The breadcrumb log names the culprit.** Each one is recorded once, with
  the holder and the duration, in `logs/breadcrumbs.log` under your orrerix
  data directory. If busy answers keep coming, that file is what to send.

**When an agent reports `internal error: … ended without a result`.** This one
is rare and it is not the same thing as a busy answer: it means the change the
agent asked for hit a bug inside orrerix and stopped partway rather than
finishing. The agent is told which read tool to check with — `list_tasks`,
`list_agents` and so on — and it should look before trying again, because the
change may have been partly applied. A crash log naming the fault is written
into `logs/` under your orrerix data directory at the same moment; that file,
with `logs/breadcrumbs.log`, is what to send. Nothing else stops: the rest of
orrerix keeps answering, and the agent can carry on with the next thing.

**When a panel says orrerix "refused that to avoid deadlocking itself".** The
same class of internal bug, reached from a button rather than from an agent.
The action you clicked did not run, it may have partly applied, and the rest of
the app keeps working — so check whatever you were changing before clicking
again. This one writes no crash log (nothing crashed; orrerix stopped itself on
purpose), so `logs/breadcrumbs.log` is the file to send. It should never
appear; if it does, that is worth reporting.

From the lifecycle panel you can:

- **Pause** the group — orrerix stops delivering prompts so its agents finish
  their turn and idle out (reversible with resume). **Pausing holds deliveries
  rather than dropping them**: nothing is typed into a pane while you're
  paused, so nobody spends tokens, but a worker's `done` report fired
  mid-pause is queued — on disk, so it survives a restart taken during the
  pause — and delivered when you resume, labelled as having waited on the
  pause rather than on a blocked pane. An agent *spawned* during a pause is
  held the same way, and resumes as the boot it is: orrerix still waits for
  its CLI to finish painting and still answers Copilot's "Enable autopilot
  mode" dialog before typing the brief, instead of pasting into a half-booted
  pane and leaving the agent sitting at a consent dialog nobody dismissed.
  Two things still refuse rather than
  wait, because both are you acting *now*: the compose strip's **Send** and a
  task's **Start** button both tell you to resume first instead of deferring
  your message to a moment you haven't picked. And the hold is not unlimited —
  each pane queues at most 8 deliveries, so a very long pause on a busy pane
  can start refusing new ones; the sender is told, and on resume the
  orchestrator is told what was refused.
- **End orchestration** — kills *every* agent in the group at once (two-click
  confirm; it's destructive). An optional **remove worktrees** checkbox also
  deletes each agent's git worktree — uncommitted changes are lost, but the
  branches (where the PRs live) are always kept.
- **Max live agents** stepper (1–12) — adjust the cap on the fly; orrerix
  persists it, audits the change, and tells the orchestrator to re-plan against
  the new ceiling. Lowering the cap below the current live count never kills
  anyone — it just blocks new spawns until attrition brings the count back under.
- **Fold panes** — the same group-wide minimize/restore as the orchestrator
  header, for reclaiming screen space.
- **Workflow row** — when the repo has a [custom agent workflow](#custom-agent-workflows)
  active, this panel names it, lists its roster, and shows the armed merge gate
  in one line (e.g. "loomux · 9 blocks · merges to the default branch require:
  rev-orch + rev-ui + rev-tests · all-pass · ci-green") — so you know whether
  an Approve can actually succeed before you click it, not after it bounces.
  If the gate
  names reviewers the current roster can't spawn, the row warns loudly instead.
- **Advanced-orchestrator toggle** — flip a repo's custom workflow on or off
  live, no relaunch: the merge gate and the roster for future spawns update
  immediately, and the orchestrator's pane gets an `[orrerix] workflow mode
  changed: …` notice so it can adjust its spawn/review strategy mid-session.
  Agents already running keep the role they were spawned under; only new
  spawns pick up the swapped roster.

## Custom agent workflows

By default a group runs the built-in four-role roster — one orchestrator,
worker, reviewer, and planner, each on the CLI/model you picked at launch. A
repo can commit `<repo>/.orrerix/workflow.yml` and declare its own instead: any
number of named blocks, each with its own capability class (orchestrator,
worker, reviewer, planner, or manager — the last being the pane *you* talk to,
see below), CLI, model, and persona, plus a **merge gate**
naming which reviewer blocks must record a `pass` verdict — enforced
mechanically by the `gh` shim — before `gh pr merge` can succeed. See
[`doc/design/workflows.md`](https://github.com/willem445/orrerix/blob/main/doc/design/workflows.md)
for the full design.

**On `.orrerix/` and `.loomux/`.** The config dir used to be `.loomux/`, and a repo
that still has one keeps working with no action: each file — `workflow.yml`,
`lessons.md`, `workflow.layout.json` — is looked for in `.orrerix/` first and in
`.loomux/` if it is not there. Orrerix never renames that directory for you; it is
committed to your repository, on your branches, and moving it is a commit only you
should make. If both exist, `.orrerix/` wins, so you can migrate one file at a time
and see each move take effect immediately.

**If your repo squash-merges, consider `also: [body-unchanged]`.** A verdict is
bound to the commit it reviewed, so a re-push re-opens the gate. The PR *body*
is not part of that commit — and a squash merge turns it into the permanent
commit message, so a body edited after a reviewer passed lands text nobody
reviewed. orrerix always records a digest of the body a verdict reviewed and
tells the orchestrator when it has moved (on a `pass`: the approval no longer
covers what would be committed; on a `fail`: the finding may already be fixed).
Adding `body-unchanged` to your gate's `also:` list also *refuses the merge*
until the reviewers whose passes are live have re-recorded against the body as
it stands. It is opt-in because it is only true of squash-merging repos; where
merges keep the PR body as discussion rather than history, leave it out.

**Keep the batches small: `max_diff_lines`.** A merge gate can also declare a
size limit — `gates.merge: { max_diff_lines: 800 }` — and orrerix refuses any
merge of a PR that changes more than that many lines (additions + deletions,
across the whole PR). The reason is the one thing this whole feature rests on:
a review nobody can hold in their head is a review that rubber-stamps, and an
oversized PR is the standard way an agent fleet defeats its own review gate.
The number is yours; orrerix never invents one. **Omit the key for no limit** —
`0` is refused rather than read as "unlimited", because a bound that bounds
nothing is a typo. You also get an advisory at `gh pr create` time, printed
into the pane of the agent that opened the PR, so a split can happen before
review effort is spent rather than after; that advisory is best-effort and
never blocks or delays the PR being created. The refusal at merge time is the
enforced half, and a PR whose size orrerix cannot read is refused too.

**Stop the line: `also: [base-green]`.** Nothing otherwise stops agents merging
more work onto a branch whose HEAD is already broken — which compounds
failures and makes the merge queue's bisect unable to say which change was at
fault. Adding `base-green` to your gate's `also:` list refuses a merge while
the HEAD of the PR's **base** branch is red, still running, or reports no
checks at all. Opt-in, and deliberately strict: "orrerix could not tell" is
never treated as green, so a repo whose CI legitimately skips some commits
(leaving a base commit with no checks on it) should not declare this — every
merge onto such a commit would be refused until something ran there.

The same strictness covers a case you would otherwise never think about: GitHub
returns a commit's check runs one **page** at a time, so a base carrying more
checks than a single page can report is treated as unreadable — not as green —
and the merge is refused. Orrerix asks for the largest page the API allows (100),
so this only bites a base with more than 100 checks on one commit; if that is
permanently true of your default branch, `base-green` cannot be enforced for it
and should not be declared.

**Route reviewers by path: `routing:`.** A gate's `reviewers:` list is the same
on every PR, which leaves a multi-lane workflow choosing between requiring every
lane on every PR and leaving the decision to prose. `routing:` makes the required
set depend on what the PR actually changed:

```yaml
gates:
  merge:
    require: all-pass
    reviewers: [rev-lead]
    routing:
      - paths: ["src/**"]
        reviewers: [rev-ui]
      - paths: ["**/Cargo.toml", "package-lock.json"]
        reviewers: [rev-deps]
```

A PR that touches only docs needs `rev-lead`. One that touches `src/` needs
`rev-lead` and `rev-ui`. One that adds a dependency needs `rev-deps` as well —
which is the "an agent quietly added a dependency" review you would otherwise be
relying on someone to remember. **Rules only ever add**: the required set is your
`reviewers:` list plus every rule that matched, so writing a rule can never make
a gate easier to satisfy. A routed reviewer is treated exactly like a declared
one from there on — its pass goes stale on a re-push, its `fail` blocks the
merge, and the refusal names it and the rule that pulled it in.

The globs are orrerix's own, not your repo's `CODEOWNERS` (that file names
GitHub users and teams; a gate names workflow blocks). They are deliberately
simple — one wildcard and one special case:

| You write | It matches |
| --- | --- |
| `src/**` | anything under `src/`, at any depth |
| `src/*.ts` | also anything under `src/` ending `.ts`, at any depth — `*` crosses `/` |
| `**/Cargo.toml` | every `Cargo.toml`, including the one at the repo root |
| `package-lock.json` | that exact path, and nothing else |

`*` matches any run of characters *including* `/`, a leading `**/` is optional,
and the match covers the whole path rather than part of it. That is coarser than
`.gitignore`, on purpose: a glob that matches too much asks for one review you
did not need, and a glob that matches too little skips one you did — so the
simple rule is the one that errs the safe way. Write file globs, not directories:
`src/**`, never `src/` (and never a leading `/` or a `..` segment) — those match
nothing at all, so orrerix refuses them at load rather than letting a rule
silently never fire.

Two things to know before you adopt it:

- **`routing:` and `require: threshold` cannot both be declared.** A threshold
  counts passes over a fixed list; routing makes the list depend on the diff, so
  together an extra lane could *supply* one of the required passes instead of
  adding one. Rather than guess which you meant, orrerix refuses the file. Use
  `require: all-pass` with `routing:`.
- **A PR whose changed files orrerix cannot list in full is refused.** GitHub
  returns a PR's file list one page at a time and orrerix can only ask for 100,
  so a PR changing more than 100 files cannot be routed — it is refused rather
  than treated as matching nothing, because "no rule fired" and "we could not
  look" must never be the same answer for a gate. Split such a PR (which
  `max_diff_lines` above probably wanted anyway).

The workflow pane shows your rules and preserves them across edits, but does not
offer a control for adding one yet — edit `.orrerix/workflow.yml` directly.

**Verdict notices are short on purpose.** Recording a verdict also types a
courtesy notice into the orchestrator's pane, so it learns the review landed
without polling for it. That notice carries the verdict, the PR, and only the
**first ~400 characters** of the reviewer's summary, followed by a pointer to
the rest — pane text becomes that agent's resident context and is re-sent on
every turn it takes afterwards, so a full copy of every summary is paid for
repeatedly. Nothing is lost: the whole summary stays in the verdict record (the
orchestrator reads it with `list_verdicts`, which is also what the merge gate
reads) and in the review posted on the PR itself.

**Opt-in, every time.** A workflow file arrives with a `git clone` — the
**advanced orchestrator** toggle is what makes a repo's workflow take effect;
off (the default), the file is never even opened. Turning it on, at launch or
live (see *Group lifecycle*, above), shows you the resolved roster — every
block, its CLI/model, and which ones carry a repo-authored persona — before
anything spawns.

**The gate belongs to the session, not to a PR's history.** Toggling the
workflow off ungates every merge from that session onward, including a PR
opened earlier while the workflow was on — a gate that outlived the toggle
that armed it would be exactly the kind of surprise this feature exists to
prevent. A human Approve grant still never opens the workflow gate by itself
(see *The task board*, above); toggling the workflow off is what actually
clears it.

**orrerix never silently arms a gate it can't satisfy.** If a workflow's merge
gate names reviewer blocks the currently-running roster can't actually spawn —
most commonly a broken or missing `workflow.yml` on a relaunch — orrerix
doesn't drop the gate to keep merges flowing; it arms the gate anyway and
shows a loud warning in the lifecycle panel, so the mismatch is something you
see rather than something a bounced merge makes you go find.

**Copilot personas: a `tools:` list must let orrerix through.** If a block sets
`cli: copilot` and points `profile:` at a `.github/agents/*.md` file, Copilot
loads that file directly — and if the file declares a `tools:` list, that list
*filters* everything the agent can reach, MCP servers included. A list that
never names loomux produces a delegate that can see the loomux server and use
none of it: it cannot report, read the task board, or be steered, and from
inside its own pane it looks like orrerix is broken. Add the server to the list:

```yaml
tools: ["read", "edit", "execute", "orrerix/*"]
```

orrerix repairs this for you where it safely can — it launches such a block from
its own stand-in for the persona, carrying every line of your file plus that
grant, and never edits anything in your repo — but it always tells you, in the
spawn reply and the audit log, which file to fix.

Three cases it deliberately leaves alone, because each is a choice you made
rather than something you forgot. It says so, and the fix is yours:

- the persona declares its own `mcp-servers:` — orrerix's stand-in would drop them;
- `tools: []`, which disables every tool — orrerix won't turn "nothing" into
  "nothing except loomux";
- the list already names loomux per-tool (`loomux/report`) — orrerix won't widen a
  scope you set on purpose.

**Picking a block's model.** The workflow pane's block form offers the same
model dropdown the launcher does, filled the same way: the CLI's own reported
models first, backed by orrerix's suggestions, plus a **custom…** entry for any
id neither list carries (a Bedrock inference profile, a gateway deployment
name, a model newer than your build). A CLI orrerix has no suggestions for and
can't get a list out of gives you that free-text box directly. Leaving the
model **unset** is a real choice with its own row: the block then runs
whatever orrerix defaults to for its kind on its CLI — `sonnet`/`opus` on
Claude Code, `auto` on Copilot, `pro` on Gemini, and on OpenCode no `--model`
at all, so your own config decides.

**Almost every setting in the file is editable in the pane.** Beside the
roster's block rows and its merge-gate row sit four more: **Intake**, **Merge
queue**, **Review driver** and **Resources** - the same `intake:`,
`merge_queue:`, `driver:` and `resources:` blocks described elsewhere on this
page. Each has an enable-toggle, and what the checkbox reads differs by what
the section's `enabled` state is. For **Intake** and **Resources** the checkbox
is the section itself: switching it on writes the block, switching it off
removes it. The **Merge queue**'s checkbox is its presence too, with a separate
dropdown for the `enabled:` line - that file has three states and a checkbox
has two. The **Review driver**'s checkbox is the `enabled:` line itself:
ticking it on writes `enabled: true` (keeping any counters the block already
declares), and unticking keeps what the block carries: a block that holds
nothing but the switch is removed whole — absent and off are the same thing to
orrerix, and deleting is tidier — while one carrying counters or comments has
`enabled: false` written into it and loses nothing. A block that declares
counters but no `enabled:` line shows
unchecked, because that is what the engine reads as off. When that line
carries its own trailing comment and a flip there would rewrite the value in
place and change it — the line's value ends in a true/false spelling and is
not already the value the flip writes — the form says so beside the toggle:
flipping the switch rewrites the value on
the line and leaves the comment exactly as written — orrerix never edits your
prose, and does not guess whether a
comment still agrees with the switch, so the note is the cue to read the line
after a flip. (A value that ends in a true/false spelling but is spelled with
uppercase letters — `True`, `FALSE` — is one the pane flags, since its reader
is lowercase-only where the engine is not; a flip there rewrites the value to
lowercase and keeps the comment.) The driver's counters
and timeouts are number fields clamped to their own declared ranges, so the
form cannot write an out-of-range value at all; what the engine then does with
a *hand-written* value outside those ranges differs by field, and the next
paragraph says which. The block form covers the
rest: `role_hint`, and `allow:` as a list of tool patterns (one row per
pattern, because a real pattern contains commas).

Two keys have no control. `authored_with:` deliberately never will — it records
which orrerix *created* the file, is stamped once, and a save must never invent
or restamp it. `board:` (WIP limits) does not have one **yet**: the pane reads
it, keeps it, and writes it back untouched when you edit anything else, so
editing your workflow in the pane is safe — you just set the limits in the text
editor for now.

Two things those forms will not let you do, because orrerix's engine would
refuse the file: write a number outside a **refused** field's range, and pair
a `role_hint` with a kind that hint does not apply to. Which bounds the engine
refuses and which it clamps is a per-family fact, stated here family by
family — read each row as its own policy, never inferred from a neighbour:

- **Merge gate** (`gate:`): `threshold` is 1 or more, refused below that — and
  a `threshold` gate is also refused above its own reviewer count, a
  validation rule rather than a range. `max_diff_lines` is 1 or more, refused
  at 0, with the fix named in the error: omit the key to declare no limit.
  Neither has a ceiling. Both have form fields whose inputs enforce the bound —
  they cannot emit an out-of-range value — while a hand-written out-of-range
  value reaches you as a finding.
- **Merge queue** (`merge_queue:`): `max_batch` is 1 or more, refused below,
  no ceiling; `checks_timeout_minutes` is 5–240 minutes, **clamped**.
- **Review driver** (`driver:`): `max_review_rounds` and `max_ci_attempts` are
  1–3 rounds each and `max_rebase_attempts` is 0–1 rebases, all **refused**
  outside; `lane_timeout_minutes`, `fix_timeout_minutes` and
  `drive_timeout_minutes` are 5–240 minutes, **clamped**.
- **Lock resources** (`resources:`): `slots` is 1–64 and `max_hold_minutes`
  is 1–480 minutes, both refused outside; at most 32 resources may be
  declared. These are the fields the inputs themselves enforce — they cannot
  emit an out-of-range value at all.
- **Board WIP caps** (`board.wip:`): each status cap is 1 or more, refused
  below — a cap of 0 is a stop, not a limit — with no ceiling, and an omitted
  status has no cap. The board has no form yet, so these reach you as
  findings.

A
value a *hand-edited* file already carries is shown as a finding instead, with
the distinction that matters spelled out — a bound orrerix **refuses** reads
as an error, one it **clamps** as a warning.

An untouched section is never rewritten. orrerix writes only what the file
declares, so opening these forms to look at them changes nothing. This
paragraph is about the **single flip**: what one click on the toggle does to
the lines around the value it changes — not the on-then-off round trip, which
is a different question with a different answer, and the two readings are kept
apart below. For the toggle itself: unticking keeps what the block carries —
a block that holds nothing but the switch is removed whole, while one carrying
counters or comments has `enabled: false` written into it and loses nothing —
and ticking on after unticking writes `enabled: true` and leaves the block's
lines byte for byte. A `driver:` block that carried no `enabled:` line at all
is the case that starts differently: turning a driver on has to add that line,
so the two clicks leave such a block declaring `enabled: false` where the file
had none.

For one flip on an existing block: **the lines around the value survive the
flip byte for byte when the flip is an in-place value replacement on a block
whose fields are on their own lines and whose value is spelled exactly `true`
or `false` and is not already the target.** What follows from it: a value that
does not end in a true/false spelling at all (`enabled: yes`) cannot be
replaced in place, so a flip regenerates the whole section and the block's
comments do not survive it; a value that already reads as the value the flip
would write (`enabled: nottrue` under a checkbox that would write true) makes
the flip a byte-identical no-op; a value spelled with uppercase letters
(`True`, `FALSE`) is replaced but not with itself — the flip writes the
lowercase spelling, so the line changes, the pane flagged the spelling, and
the comment survives; and a block written in flow style
(`driver: {enabled: true, ...}`) has no
field lines to replace on, so a flip rewrites it in block style - the model is
preserved, the prose is not. The note stays silent wherever the promise would
not hold, and the pane flags every value the pane's own reader cannot read
(the lowercase-only list above), which is every one of these shapes on the
file's own terms. The round trip — the toggle on and then off, the gesture the
pane actually offers — follows from the same rule rather than needing its own:
where each flip preserves the lines around its value, the second flip is the
first one in reverse, so on-then-off restores the file byte for byte; where a
flip regenerates or rewrites the section, the file that comes back is not the
one that left, and the shapes above are the ones where that happens. Unticking
never deletes configuration either: a block
carrying counters or comments is kept, with the switch written as off, and its
comments are kept byte for byte except the switch's own line.

**Removing a section is a different gesture from the toggle, and the driver
block is the one that has it.** A red button at the foot of the driver form
deletes the `driver:` block whole — the switch, every counter, and the block's
comments — behind its own confirmation that names what is discarded. This is
deliberate destruction, not part of the toggle's round-trip promise, and it
exists because a `driver:` block makes the file unloadable on any orrerix
build old enough to refuse the key (`deny_unknown_fields` on the workflow
root, verified against v1.3.0-beta1): removing the block is how the file loads
again. The toggle will not do this to you, and the button does nothing until
you confirm it.

**Reviewer diversity across models.** A block's `cli`/`model` are set
per-block, so nothing stops a reviewer lane from running on a different
CLI/model than the one that wrote the code — a second model tends to catch a
different class of defect than the one already primed on its own output.
Worth considering for any reviewer-heavy workflow; orrerix's own dogfood
`.orrerix/workflow.yml` notes the same above its reviewer blocks.

**Cheap lanes ahead of the expensive one.** A reviewer lane does not have to be a
reviewer. Because `cli` and `model` are per-block, you can put a small, fast model
on a lane whose whole job is running a fixed checklist of shell commands — is the
evidence the PR body claims actually in it, does a cited CI run belong to the
current head, does the diffstat match, is a forbidden import present — and name it
in `gates.merge.reviewers` alongside your real reviewer. The strong lane then reads
those results instead of re-deriving them, and spends its attention on the things
no command can settle. Two rules make it work: write the cheap lane's persona as a
numbered checklist with the exact command on each line rather than as a description
of what to look for, and tell it to fail **only** on the absence of something it can
quote, escalating anything it cannot decide. A small model asked for judgment gives
you unreliable judgment; asked what a command printed, it is accurate. Be clear-eyed
about the trade: a cheap lane can pass a check it should have failed, so the strong
lane stays in the gate and stays the bar — the cheap lanes buy its time, they do not
carry its verdict. orrerix's own repo keeps three such lanes as worked examples —
`qr-evidence.md`, `qr-tests.md` and `qr-constraints.md` in `.github/agents/`, each a
numbered checklist written exactly this way, and worth reading before you write your own.
Its `.orrerix/workflow.yml` does not declare them today (its cheap tier is one
*iterating* reviewer rather than fixed-checklist lanes), which is the other half of the
trade: the shape is worth having in the drawer, not necessarily in every roster.

If what you want instead is an opinion on *some* PRs — a design review before code
is written, a premortem on something risky — that is the opposite shape and wants
`kind: planner` + `role_hint: advisor`, not a reviewer block. See **Adding a second
lens** below. The question that separates them is not how valuable the opinion is,
it is **does this run on every PR?** A checklist does, and belongs in the gate; a
lens does not, and a reviewer block would both run it every time and hold every
merge shut waiting for it.

### Running a block on another machine: `remote:` (not usable yet)

A block can name an abstract **label** for the machine its agent should run on
over SSH:

```yaml
blocks:
  - id: builder-remote
    kind: worker
    cli: claude             # required, and spelled out — see below
    remote: buildbox        # a label, not an address
```

**Right now this key does nothing.** Orrerix parses it, validates it, and keeps
it across a save — and then spawns the block locally, exactly as if you had not
written it. It becomes real in two later steps: binding the label to an actual
host (the operator side, [#1458](https://github.com/willem445/orrerix/issues/1458))
and the remote spawn path itself
([#1459](https://github.com/willem445/orrerix/issues/1459)). The key ships first,
on its own, because it is the part everything else has to agree with.

**The label is a name you choose, never an address.** There is deliberately no
`host:`, `port:`, `user:`, `identity_file:` or ssh-options key — writing one does
not "not work", it fails the whole file at load. `workflow.yml` is committed to
your repository, so anyone who can open a pull request can edit it; a
repo-authored hostname would let that person point execution at any machine you
can reach. So the repo file only ever *selects* a name, and you — the operator,
outside the repo — decide which host, which account and which clone path that
name means. Same seam as personas: a repo picks from what you defined, and can
never mint something new.

Two rules the parser enforces, both of which fail the file rather than warn:

- **Not on an `orchestrator` or `manager` block.** Those two run where you are,
  and that is load-bearing: the orchestrator holds your orchestration state, its
  `gh` operations and the merge gate, and a manager pane is the thing you type
  into. Put `remote:` on the blocks the orchestrator spawns.
- **`cli: claude`, written on the block.** A remote agent's session has to be
  identified by an id orrerix minted before the spawn, and Claude is the only CLI
  today that accepts one — the others recognise a session by scanning a store on
  the local disk, which a remote CLI's disk is not. Leaving `cli:` off is refused
  too: an omitted CLI inherits the group default, which is picked at launch, so
  orrerix cannot tell at load time whether the block would end up on Claude.

Both rules are deliberately strict in the direction that is cheap to change
later: relaxing a refusal costs nobody anything, while adding one to a key
people have already written into committed files breaks their workflows.

The label itself is letters, digits, `-` and `_`, at most 64 characters, and not
starting with `-`. A handful of names Windows reserves for devices (`CON`,
`NUL`, `COM1`…) are refused too, in any capitalisation — on Windows those are
not file names at all. It is refused rather than cleaned up, so two spellings can
never end up meaning one machine — and it is **case-sensitive**, so `buildbox`
and `BuildBox` are two different labels, not one written two ways. A block with
no `remote:` key is a local block and is completely unaffected — which is every
block in every workflow file written so far.

One thing to know before you write a label into a committed file: **a label
nobody has bound yet is not an error today.** It parses, saves and round-trips,
and the block runs locally. That is deliberate for now — the key ships before
the binding does — but it means a name written today is a name an operator may
bind differently later. Whether an unbound label should say so at launch is a
decision for the binding step ([#1458](https://github.com/willem445/orrerix/issues/1458));
until it lands, treat a label as a note to your operator rather than as a
setting that is in force.

The full design note for remote roles lands with the rest of the feature
([#1462](https://github.com/willem445/orrerix/issues/1462)); the plan it is being
built from is on [#1436](https://github.com/willem445/orrerix/issues/1436).
### A manager pane — the human's own interface

Every block above is an agent doing work. A `kind: manager` block is not: it is
the pane **you** talk to. Project discussion, "how is it going", and the
half-formed feature idea you have not written down yet all belong there, and its
job is to turn the last of those into something the team can build correctly the
first time. [The manager pane](features/manager.html) is the page for using one;
this section is how to declare one.

```yaml
version: 1
blocks:
  - id: orchestrator
    kind: orchestrator
    cli: claude
    model: opus

  - id: manager           # at most one per file; a second is a parse error
    name: Manager
    kind: manager
    cli: claude
    model: opus

  - id: worker
    kind: worker
    cli: claude
    model: sonnet

  - id: rev-lead
    kind: reviewer
    cli: claude
    model: opus

gates:
  merge:
    require: all-pass
    reviewers: [rev-lead]  # never the manager — it records no verdict
```

What that block gets you, and what it deliberately does not:

- **No fleet traffic is ever delivered into that pane.** No notices, no relays,
  no status lines, no reports — your conversation with the manager is yours.
  Exactly two things orrerix itself writes there, and both are about the pane
  rather than about the work: the kickoff that starts the conversation, and — if
  the CLI compacts mid-session — one re-grounding notice that hands the manager
  back its own directive ledger, so a direction you gave survives a compact
  nobody was warned about. Everything else is refused at the front door. News
  reaches it by **pull**: the orchestrator posts milestones into a durable
  mailbox, and the manager reads that mailbox at the start of each of its turns,
  which is the next time you speak to it. An **unread-mail chip** on the pane
  header tells you when something is waiting.
- **It holds no authority you have not used yourself.** No spawning or killing
  panes and no review verdicts — those are structural, denied at the tool level.
  No branches, commits or PRs either: orrerix runs the pane under a containment
  tier that denies its editing tools, and the rest is its instructions, which is
  why the designed path has the *orchestrator* file the issue your brief becomes.
  It relays; the orchestrator decides.
- **It does not start work.** A brief it grooms with you becomes a GitHub issue,
  and your own label on that issue is still the only thing that hands it to the
  fleet.
- **It costs no delegate slot.** A manager is exempt from `max_agents`, from the
  idle reaper and from the stall watchdog — it is idle whenever you are not
  talking to it, which is most of the time, and that is not a fault.
- **It may not be a gate reviewer**, and `prompt:`, `profile:` and `allow:` are
  parse errors on a manager block — the same rule an `orchestrator` block
  follows. Its instructions are orrerix's, not the repo's.
- **The group works without it.** Close the pane and everything behaves exactly
  as it does for a group that declares none: the orchestrator takes your input
  in its own pane, as it always has. Nothing reopens the pane automatically, on
  purpose — closing it is something you are allowed to do, and orrerix cannot
  tell that apart from a crash. The group panel says *manager declared · not
  open* while it is gone, and the session browser brings it back.

**A manager is only ever declared, never inherited.** Adding a `kind: manager`
block to the file gives a manager to *fresh* launches of that repo. A group that
already exists — including a dormant one you reattach — keeps the roster its own
launch approved and never re-reads the file, so it will not gain one on resume.
Launch a new group to pick up the change.

**`role_hint: liaison` is superseded by this.** The hint still parses and a repo
that uses it keeps working unchanged; the workflow pane now marks it as
superseded and the launcher preview badges it `LIAISON (SUPERSEDED)`. Write
`kind: manager` in a new file: a hint on a reviewer block cannot express what
this class is — a pane no fleet traffic is delivered into, with a mailbox of its
own and a capability set that is not a reviewer's.


### Turning on the merge queue

A `merge_queue:` block, beside `gates:`, opts the repo in:

```yaml
merge_queue:
  enabled: true              # default false — absent block means the feature is off
  max_batch: 3               # how many approved sub-PRs one batch may carry
  checks_timeout_minutes: 60 # how long to wait for the batch's checks before
                             # calling it unverifiable
```

With it, the orchestrator stops hand-merging approved sub-PRs onto the integration branch
and hands them to the queue instead — one batch is tested *as a combination*, and on red the
queue attributes the failure to a single PR rather than leaving someone to guess. With no
block, none of that exists and merges work exactly as they always did.

Three things worth knowing before you enable it:

- **It never touches your default branch, and never grants what your review gate would not.**
  It lands only on an integration branch, and it re-checks the same reviewer verdicts your
  `gates:` block already declares — at batch build *and* again at the moment of submit, so a
  `fail` recorded in between still stops the landing. It is strictly additive to the gate.
- **`checks_timeout_minutes` is a backstop, not the mechanism.** A batch normally resolves
  when its checks do. The timeout exists so a repo whose CI never attaches surfaces as
  **unverifiable** — loudly, with nothing landed — instead of a batch sitting pending forever.
- **Adding the block is not inert to older orrerix builds.** The workflow file rejects keys it
  does not recognize, so on a build that predates the merge queue, `merge_queue:` fails the
  parse of the **whole file** - your `gates:` included - rather than being ignored. That is
  deliberate (a key the build doesn't understand means you believe a policy is in force that
  isn't), but it means everyone sharing the repo wants to be on a build that has it.

### The review driver's `driver:` block

A `driver:` block, beside `merge_queue:`, declares how hard the engine-driven review-loop
driver (#1778) may work when it drives a PR through review and CI on the orchestrator's
authority:

```yaml
driver:
  enabled: true
  max_review_rounds: 3
  max_ci_attempts: 3
  max_rebase_attempts: 1
  lane_timeout_minutes: 60
  fix_timeout_minutes: 60
  drive_timeout_minutes: 240
```

Every number in that example is its field's own default, so a block naming only
`enabled: true` behaves exactly like the one above. `enabled:` is the one line the
example does not show at its default - it defaults to **false**, and an absent
`driver:` block means the feature is off.

<!-- pinned-to-schema: sections.driver - test/docsdriverbounds.test.ts (#1872) -->

| Field | Range | Default | Outside the range |
| --- | --- | --- | --- |
| `enabled` | — | false | — |
| `max_review_rounds` | 1–3 | 3 | refuse |
| `max_ci_attempts` | 1–3 | 3 | refuse |
| `max_rebase_attempts` | 0–1 | 1 | refuse |
| `lane_timeout_minutes` | 5–240 | 60 | clamp |
| `fix_timeout_minutes` | 5–240 | 60 | clamp |
| `drive_timeout_minutes` | 5–240 | 240 | clamp |

**refuse** fails the parse of the whole file: a value outside the range is a policy
you believe is in force and is not, so orrerix will not load the file at all.
**clamp** pulls the value into range and reports the edit as a warning - the three
timeouts are backstops on a wait, and every notify-TTL wait in orrerix clamps the
same way.

A repo may run a *tighter* loop than the orchestrator template promises, never a looser one:
the driver acts on the orchestrator's authority, and a config file that raised the bound
would be loosening the orchestrator's own invariant. The same forward-compat warning the
merge queue carries applies here too: on a build that predates the block, `driver:` fails
the parse of the whole file rather than being ignored.

The block **enables** the feature; it can never start, target or widen a drive - no drive
exists until an orchestrator makes its own role-gated `drive_review` call naming one PR.
(The workflow pane edits this block too: an enable-toggle whose state is the `enabled:`
line, plus number fields bounded to the ranges shown above - #1869.)

### Setting up a cross-model reviewer

`cli:` accepts `claude`, `copilot`, `gemini`, or `opencode`. So a workflow
whose worker runs on Claude gets a genuinely different model family
reviewing it by naming one on a reviewer block:

```yaml
version: 1
blocks:
  - id: worker
    kind: worker
    cli: claude
    model: sonnet
  - id: rev-gemini          # a second opinion from a different model family
    kind: reviewer
    cli: gemini             # model: defaults to gemini's `pro` reasoning tier
gate:
  require: [rev-gemini]
```

You need the CLI itself installed and logged in — orrerix spawns `gemini` from
your `PATH` the same way it spawns `claude`. A CLI named by a workflow block is
**not** checked before launch (only the CLIs picked in the launcher's own role
dropdowns are), so if it isn't installed the pane still opens, prints the
shell's not-recognized error (on Windows, `The term 'gemini' is not
recognized…`), and exits; the orchestrator is then told that agent died. Nothing else is needed: the reviewer's orrerix
tools (including the `pass`/`fail` verdict the merge gate reads) are wired up
per agent, and its containment is generated per agent too, so a gemini
reviewer is denied the file-editing tools exactly like a Claude one.

Two differences worth knowing before you pick gemini for a lane:

- **`allow:` doesn't apply to a gemini block.** Those patterns are
  Claude/Copilot tool-matcher strings. A gemini block runs with its class's
  baseline and can't be widened.
- **No compact nudge.** orrerix's context-pressure nudge types `/compact`,
  which gemini doesn't have (its command is `/compress`), so gemini agents
  are skipped rather than sent a command that doesn't exist.
- **No session history features.** orrerix can't resume a gemini session or
  read its transcript — gemini mints its own session ids rather than
  accepting one, so there's nothing for orrerix to record and reopen later.

`cli: opencode` is a fourth choice, and it runs opposite gemini on that last
point:

- **`allow:` doesn't apply to an opencode block either.** Its permission keys
  (`edit`, `bash`, …) are a different namespace from the Claude/Copilot
  tool-matcher strings `allow:` speaks, so an opencode block also runs with
  its class's baseline and can't be widened — same decision, same reason, as
  gemini's arm.
- **No compact nudge.** opencode isn't on the short list of CLIs orrerix will
  paste `/compact` into (claude and copilot only), so an opencode agent's
  context management is left to the CLI itself, same as gemini's.
- **Session history *does* work.** opencode has no `--session-id` to hand a
  pane up front, so orrerix learns which session is the reviewer's after it
  starts rather than minting one — but once bound, that session resumes and
  its transcript reads back like any claude or copilot one.

**Why not codex?** codex can't deny its editing tool by name, and its sandbox
is all-or-nothing — strict enough to block the tests and `gh` a review needs,
or open enough to let the reviewer rewrite the code it's reviewing. A reviewer
that can't be contained would quietly weaken the merge gate, so orrerix refuses
the pairing rather than shipping it.

Turning it on live shows the same resolved-roster confirm (name, blocks, any
declared gate) the launcher's own preview shows at launch time; turning it off
confirms that future spawns fall back to the built-in roster on your default
CLI (per-role CLI overrides picked at launch aren't separately retained, so an
off→on→off round trip rebuilds the roster from your default CLI rather than
restoring them).

### Adding a second lens

A repo that wants a design-review opinion before code is written, or a
premortem pass on an unusually risky PR, doesn't need new capability to get
there — `kind: planner` + `role_hint: advisor` already gives a block the
right *shape*: read-only, reports with `report("done", ...)` and exits
rather than holding a pane open, and never records a verdict. What it does
**not** already give you is the trigger. Every sentence orrerix itself
generates about an advisor block — the orchestrator's own kickoff note, the
worker's counterpart, and the non-overridable addendum the advisor block
receives regardless of its persona — is keyed to *the team being stuck*, not
to plan intake, PR size, or risk. A workflow file cannot widen that:
`prompt:`/`profile:`/`allow:` on the `orchestrator` block are a parse error
(see "The orchestrator block is loomux-owned" in
[`doc/design/workflows.md`](https://github.com/willem445/orrerix/blob/main/doc/design/workflows.md)),
so there is no way to tell your orchestrator "consult `design-review` at
plan intake" or "spawn `premortem` when the diff looks risky" from the file
itself. In practice, what fires a plan-intake or high-risk consult today is
a human asking for one, or the orchestrator's own judgment reading the
roster — not anything this pattern wires up mechanically. Declare as many
advisor blocks as you want, each with its own persona; just don't expect the
trigger to come for free.

**Why not `kind: reviewer`.** orrerix's own built-in orchestrator template
makes the orchestrator run **every** reviewing block — every `kind: reviewer`
block except one hinted `role_hint: liaison` — on **every** PR,
unconditionally, whether or not that block is named in a merge gate.
Declaring the lens as a reviewer costs a pane on every PR it was never meant
to see; naming it in a gate on top of that adds a second, separate cost — it
can then hold every merge shut until someone spawns it and it passes. An
advisor block avoids the first cost outright (nothing spawns it
automatically) and the second is structural, not just a discipline you have
to keep: `gate_reviewer_error` refuses, at parse time, any gate naming a
non-reviewer block — an advisor cannot even be *named* in one, the file
would not load. The orchestrator only spawns it when it judges the moment
warrants the extra look (see the trigger caveat above).

Two ready-made personas, modeled on this repo's own
[`.github/agents/advisor.md`](https://github.com/willem445/orrerix/blob/main/.github/agents/advisor.md):

```markdown
---
name: design-review
description: >
  A read-only advisor consulted on a plan before it's built, or a PR whose
  diff crosses this repo's size threshold — a second opinion on the shape of
  the change, not a correctness review. Investigates and reports; never
  merges, spawns, or records a verdict.
kind: planner
mode: replace
---
You are consulted on a design question: a plan under intake, or a PR the
orchestrator judges large or consequential enough to warrant a second look at
its shape, not just its correctness.

## What you do

1. **Investigate READ-ONLY.** Read the plan or diff, the issue thread, and
   any design notes. You cannot write a file, branch, or push — the planner
   capability class denies those at the CLI level regardless.
2. **Answer one question: what alternative did this implicitly reject, and is
   the choice defensible?** Name the alternative, not just "this looks fine"
   — a design review that agrees with everything isn't a second opinion.
3. **Report and exit.** `report("done", "<your assessment>")` is your one
   deliverable — lead with the alternative and your verdict on it. Skip
   anything that didn't change your answer.

## What you never do

No authority beyond the assessment: never merge, never spawn another agent,
never record a review verdict, never edit or push. The orchestrator decides
what to do with your advice, including ignoring it.
```

```markdown
---
name: premortem
description: >
  A read-only advisor consulted on a PR the orchestrator judges high-risk —
  a migration, a security-sensitive surface, a persisted-shape change.
  Investigates and reports; never merges, spawns, or records a verdict.
kind: planner
mode: replace
---
You are consulted on a PR the orchestrator has flagged as high-risk. Your job
is to imagine it has already shipped and failed, and work backward.

## What you do

1. **Investigate READ-ONLY.** Read the diff, the issue thread, and the tests
   it added. You cannot write a file, branch, or push — the planner
   capability class denies those at the CLI level regardless.
2. **Answer three questions:**
   - **Premortem** — two concrete ways this fails in production that no test
     in the PR catches, or an argued "none".
   - **Resource envelope** — if the diff touches an unbounded input, the
     largest realistic size × invocation frequency × allocation/IO, and
     whether anything bounds it.
   - **Operational futures** — what happens at 10× load, and on an
     upgrade/rollback across whatever persisted shape the diff touches.
3. **Report and exit.** `report("done", "<your findings>")`. An empty answer
   to one of the three questions is a finding in itself — say so rather than
   skipping it.

## What you never do

No authority beyond the findings: never merge, never spawn another agent,
never record a review verdict, never edit or push. The orchestrator decides
what to do with your findings, including ignoring them.
```

`workflow.yml` declares both the same way as any other advisor block:

```yaml
blocks:
  - id: design-review
    name: Design review
    kind: planner
    role_hint: advisor
    profile: .github/agents/design-review.md

  - id: premortem
    name: Premortem
    kind: planner
    role_hint: advisor
    profile: .github/agents/premortem.md

edges:
  - { from: orchestrator, to: [design-review, premortem] }
```

`spawn_agent` is orchestrator-only for every block, advisor or not — a worker
that gets stuck is told to `message_orchestrator` and ask for a consult rather
than spawn one itself — so both lenses hang off the orchestrator in `edges:`,
never off a worker or reviewer. (One implementation detail worth knowing: the
orchestrator's auto-generated "consult the advisor" kickoff sentence is built
by `role_hint_block`, which the engine's own doc comment says picks the
*first* block carrying a given hint on purpose — so with two advisor blocks
declared, that sentence names only the first. The second is still listed
under "Your delegates" with its own persona, and is spawnable by
`block: "premortem"` exactly the same way — it just isn't individually
called out by that one sentence.)

**The residual.** An advisor's report can never satisfy or block a merge gate
— not just because it never calls `review_verdict`, but because a gate
cannot even name it (see the structural point above), so the orchestrator
reads its advice and dispositions it like any other input, the same as a
human's. A repo that wants these questions to actually gate a merge, not just
advise on one, puts them in its **standing reviewer persona** instead, as
fixed headings its review body must carry (this repo's own
`.github/agents/rev-lead.md` does exactly that for its own question set):
that shape buys enforcement, at the cost of running on every PR rather than
only the ones that need it — and it needs no trigger caveat, either, since a
reviewer that's named in the gate is spawned every time by construction.

### Proposed lessons come with their evidence

A workflow can declare a **process-pro** block — a worker that runs after a
PR merges, reads that session's record cold, and opens a normal PR proposing
a durable lesson (an entry in `.orrerix/lessons.md`, a `.claude/skills/` entry,
a `CLAUDE.md` rule). Like every other agent it proposes and stops; it never
merges anything, its own PR included.

**Unlike every other agent's PR, though, this one does not come to you.** The
learning loop is meant to be self-managed, so a process-pro PR is *orchestrator-
owned*: the orchestrator reviews it and then merges or closes it itself, rather
than parking it in your merge queue. That is deliberate — a loop whose whole
output is "here is another PR for the human to read" costs you more attention
than the lessons are worth, and stops running the week you get busy. The bar it
merges on is the ordinary one, not a lower one: the group's review passed, CI
green, findings settled. Only the *owner of the decision* changes.

Two things that follow. Closing is a normal outcome — most sessions produce no
durable lesson, and an orchestrator that merges every proposal is not filtering.
And you are not out of the loop: each merge is audit-announced and recorded on
its board task, so the lessons that landed while you were elsewhere are there to
read back, and a lesson you disagree with is a curation PR away from being gone.

The thing worth knowing when you *do* read one is what it is allowed to claim.
Anything the process-pro writes into those files is
inlined into every future session's context, so a wrong or trivial lesson is
a cost you keep paying — which makes "was this actually a recurring problem,
or did one agent have one bad afternoon?" the question the review turns on.

orrerix answers it mechanically rather than leaving it to the agent's opinion
of itself. Each piece of friction it found carries a **recurrence** count:
how many *other* sessions in the group hit the same wall, and which ones.
So a proposal should read like *"three sessions hit this — `w-2`, `w-7`,
`w-9`"*, and you can go look at those sessions. A proposal from a wall only
one session ever hit is supposed to say so and argue why it will recur anyway
(a documented rule somebody missed, say) — if it doesn't, that is the cue to
close it rather than merge a lesson built on one bad afternoon. The orchestrator
is the one holding that cue on a normal run; it is also what you check for if
you go back over what the loop merged.

Two caveats the proposal should carry when they apply, because they change
what the number is worth: a brand-new group has no earlier sessions to
compare against, so a `0` there means *nothing to compare*, not *never
happened*; and only a bounded number of recent sessions are read, so on a
long-running group a count is a floor rather than a total.

### What actually reaches a kickoff from `.orrerix/lessons.md`

The lessons file is injected into every orchestrator kickoff, and only about
**4 KB of it** is — a deliberate bound on how much repo prose lands in an agent's
context (agents get the file as *data to weigh*, never as instructions). Files
outgrow that, so what happens at the edge is worth knowing:

- **Whole entries are dropped, oldest first.** The unit is a `## ` heading and its
  body; you never get half an entry, or a body injected under the wrong heading.
  A `## ` line inside a fenced code block doesn't count — an entry can quote a
  heading (including this page's `[pinned]` example) without being split in two.
- **The injection says what it dropped.** The first line names each evicted entry
  by heading, so "the rule about X isn't reaching agents any more" is visible in
  the kickoff instead of being something you find out the hard way.
- **`[pinned]` in a heading keeps that entry.** Put it in the `## ` line — e.g.
  `## Never resize the PTY for a UI feature [pinned]` — and eviction takes it last,
  after everything unpinned, whatever its position in the file. Use it for the
  entries whose absence is a real failure; if the pinned entries alone exceed the
  cap, the oldest pin is dropped too, so pinning everything is the same as pinning
  nothing.

Dropping is not deleting: the file is untouched on disk. When entries start falling
out, the fix is a curation PR that retires the stale ones — the same review path any
other change to the file takes.

### Watching the merge queue

A repo can turn on a **merge queue** (`merge_queue:` in its `.orrerix/workflow.yml`) so a
batch of approved sub-PRs is tested *together* on a scratch ref before any of them reaches
the integration branch — the combination is what gets a gate, instead of each PR getting one
and nobody checking the pile. The queue runs in orrerix itself and lands only on an
integration branch, never on your default branch; see
[`doc/design/merge-queue.md`](https://github.com/willem445/orrerix/blob/main/doc/design/merge-queue.md)
for the design.

The lifecycle panel shows what it is doing, and nothing more — **the row is read-only**.
There is no button here to enqueue, cancel, or land anything: the queue is host-run, and the
orchestrator drives it through its own tools. What you get is the branch the queue is landing
on, the batch in flight (with the draft PR whose checks it is watching), and one line per
queued PR:

- **queued** — waiting for a batch. If a PR was rebased after its reviewers passed, its
  approvals no longer cover its head, so it is queued *and blocked*, and the row says why.
  It becomes eligible again the moment a re-review covers the new head; nothing merges
  unreviewed.
- **in the batch being built · waiting on batch CI · landing** — in flight.
- **in the bisect · kicked back** — the batch went red and the queue is attributing (or has
  attributed) it. A kicked-back PR is not going to land as things stand; its own PR carries a
  comment naming the failing check.
- **landed · cancelled** — done with.

Two things the row will never do quietly. If more PRs are queued than fit, it says *showing 6
of 12 entries* rather than showing you six and letting them read as all of them. And if
orrerix cannot read the queue's state file at all — a torn write, or a file written by a newer
build — it says **that**, loudly, instead of drawing an empty queue: "nothing is queued" and
"orrerix can't read the queue" are the same picture otherwise, and only one of them means
your PR is fine.

**How quickly it moves.** The queue is driven by orrerix's background poller, which wakes
every 30 seconds and advances **one group's queue per wake** — so a batch normally starts
within a wake or two of the last PR being queued, and a batch whose CI has just gone green
lands about that fast too. If several groups have live queues they take turns, oldest first.
That bound is deliberate: the same loop delivers every agent's `notify_when` CI notice, and a
driver that serviced every group on one wake would hold those up. If something external
fails — the remote is unreachable, a push is rejected — the queue holds that group off for
five minutes rather than retrying every wake, so you get one notice about it rather than ten.

The row is absent entirely until a group actually has a queue, which is the default: no
`merge_queue:` block means the feature is off and nothing about your group changes. See
[Turning on the merge queue](#turning-on-the-merge-queue) for the block itself.

**What the orchestrator does with it.** It queues an approved sub-PR rather than merging it,
reads the queue's state, and can pull a PR back out — three tools, no merge authority beyond
what it already had. When a batch goes red it gets one notice naming the culprit, and a
comment lands on that PR with the failing check and the batch's sibling set. orrerix
deliberately does **not** brief the PR's author itself: attribution is mechanical, but
deciding who picks it up — and whether to resume that worker or spawn a fresh one — is a
judgment call, so it stays the orchestrator's.

Two honest limits, both of which the culprit comment states in as many words. Bisect isolates
**a** culprit, not necessarily **the** culprit: when two changes are each fine alone and only
fail together, the search blames whichever one the split isolated, and the comment names the
siblings so you can see that rather than being told a half-truth confidently. And a batch that
comes back **unverifiable** implicates no PR at all — the checks never resolved, nothing
landed, and the thing to look at is your CI.

### Watching a review drive

A repo can also turn on a **review driver** (`driver:` in its `.orrerix/workflow.yml`) so the
worker-reviewer rounds a PR goes through — wait for CI, brief the reviewer lanes your gate
requires, hand a `fail` or a red run or a conflict back to the worker, repeat — run in orrerix
instead of costing the orchestrator a turn each time. It runs on the same 30-second poller the
merge queue does, under the same bound: **one group per wake, oldest first**. See
[`doc/design/review-driver.md`](https://github.com/willem445/orrerix/blob/main/doc/design/review-driver.md)
for the design. The `driver:` block's own fields are documented with the other workflow blocks by
#1784, which lands beside this — until it does, this page describes what the driver *does* and not
what you may set.

**Nothing starts by itself.** The block only *enables* the feature. No PR is driven until the
orchestrator makes a deliberate per-PR call naming that PR and the worker session that owns it —
and in particular a drive does **not** start when a worker reports it is done, because the PRs
where a drive would be wrong are ordinary ones: a scratch PR, a release bump, a PR you said you
would read yourself.

**The driver never merges, and never grants what your gate would not.** It cannot merge, push,
mark a PR ready, delete a branch, edit or relabel a PR or an issue, write a merge grant, kill a
pane, or record a verdict — only a reviewer's own verdict opens your gate, and a finished drive is
never a substitute for one. It reads GitHub and it types templated text into panes orrerix already
owns. What is genuinely new, and worth saying plainly: **orrerix now spawns a reviewer and resumes
a worker on its own initiative, with no orchestrator turn in between.** Every one of those actions
is in the audit log, marked with the orchestrator it acted for.

**One thing changes about what you see in the orchestrator's pane.** While a PR is being driven,
its delegates' status reports and recorded verdicts go to the driver instead of appearing there —
that is the point, and it is most of the saving. One qualification, because you will otherwise
notice it and wonder: interception is keyed on a pane orrerix has recorded, and it records the
worker's pane when it first hands the PR back. Until that first hand-back — while the drive is
still waiting on CI or on a reviewer — a `report` from the worker still lands in your pane as it
always did. Nothing in the drive reads it, so nothing is lost; it is simply not yet silent. Two things keep it from being a black box: every
consumed event is in the audit log (as `rd-consumed`, naming the kind, the agent and the PR —
*consumed* is a different word from *dropped*), and a delegate's own `message_orchestrator` line is
**never** intercepted. If a reviewer or a worker has something to say that is not a status change,
you still see it, unchanged, and the drive then stops so someone reads it.

**A driven worker that reports progress gets answered, in its own pane.** A drive advances on a
worker's `report(done)` — including the case where the fix moved nothing to push, such as a PR-body
edit or a finding the worker answered rather than changed code for. A `report(progress)` moves it
no further, so instead of waiting the fix timeout out the driver types one line back into that
worker's pane saying so. Once per hand-back, never to you.

**A drive stops, it does not drift.** There are fifteen ways out and each produces exactly one
line in the orchestrator's pane: the gate being satisfied, the drive being cancelled — by you, or
by orrerix on its own when it sees the PR has been closed — or one of
thirteen holds — a counter reaching INVARIANT 9's bound, a reviewer escalating, a lane or a worker going quiet past its timeout,
the drive itself getting old, a reviewer requirement orrerix could not compute, a gate file it
could not read, a worker that reported blocked, a delegate messaging the orchestrator, this group's
live-delegate cap refusing the pane a hand-back needed, or a fix that could not be handed back to
its worker. The last of those quotes what actually refused rather than
diagnosing one cause: the session may no longer resolve, the block it was minted under may no
longer be declared in this group's roster, or the pane the driver resumed may have opened and then
exited without saying anything. The cap one (`cap-refused`) is deliberately separate from it,
because its remedy is: the recorded session is fine and what is exhausted is a *slot*, so freeing
one — `kill_agent` on an idle delegate, and the notice names which are idle — is what clears it,
not re-pointing the drive at a different session. It is the one hold reason named here by its own
word, because it is the one whose remedy is a different action from its neighbour's. **A hold is parked, not finished**: it keeps what it has spent, so
resuming it does not silently grant a fresh budget, and clearing the counters is a separate,
audited decision.

**A hand-back reopens the worker's own session under the worker's own block** — the persona and CLI
that session was minted with, never the roster's default worker block. That matters wherever a
workflow file declares more than one worker: a session belongs to one CLI, and reopening a Claude
transcript under an opencode block does not produce a different persona, it fails to open at all.
If the block a session was minted under is no longer declared, the drive holds and names it rather
than quietly resuming the work as somebody else. Where that session already has a live, idle pane
running that same block, the driver types the brief into it instead of opening a second pane on one
conversation — so a review round normally costs no new delegate slot at all.

**Drives and the merge queue do not overlap, and the exclusion is deliberately not symmetric.** A
PR with a LIVE drive cannot be queued, and a PR with a queue entry that has not finished cannot be
driven; each refusal names the other holder. The asymmetry is the `held` case: a parked drive
moves nothing and cannot race a batch, so it does **not** block queuing — but resuming it under a
live queue entry is refused, so the two loops still never run on one PR at once. The intended order is serial: let the drive reach a satisfied gate, decide what to do with
the findings — that decision stays the orchestrator's, and the driver never makes it — and *then*
queue.

**If orrerix cannot read its own drive record** — a torn write, or a file from a newer build — it
says so, loudly, and stops driving that group rather than guessing. It never repairs the file and
never deletes it. "Nothing is being driven" and "orrerix can't tell what is being driven" are the
same picture otherwise, and only one of them means your PR is fine.

With no `driver:` block none of this exists and nothing about your group changes.

## Guardrails

Enforced by orrerix, not the model:

- a cap on live agents (≤12, set at launch and adjustable live);
- models pinned per role at launch;
- the permission mode fixed at group creation (native auto mode or acceptEdits —
  never bypass).

### Compact-nudge

The orchestrator pane lives for the whole session and every turn re-reads its entire
history — it's typically the biggest token consumer in a group. Orrerix can drive Claude
Code's own `/compact` for it at a natural lull: once an eligible pane has been idle at its
input prompt (the same output-quiet signal the watchdog and idle-tick already read — never
mid-turn) past a configured window, orrerix pastes `/compact` for it exactly like any other
prompt delivery — no PTY resize, no new agent capability — and it never overwrites text
you're mid-typing (a held nudge is silently skipped, not queued; it just tries again at the
next natural lull).

Off by default. A group opts in with a quiet-window (minutes) and, optionally, which roles
are eligible — the orchestrator only, by default, since workers are short-lived and rarely
worth compacting. `/compact` is a Claude Code built-in, so the nudge only ever fires for
Claude Code panes.

**The timed nudge also checks context is actually full before it fires — a smart default, no
setup needed.** Quiet is not the same signal as full: on its own, a quiet-window nudge fires
at the right *moment* but the wrong *context level*, compacting a pane at 20-30% full and
paying a whole re-grounding cycle for one that wasn't running out of room. So once you enable
the quiet-window (above), a minimum context floor is on **automatically** at a sensible default
(50%) — nothing to configure. Three states, if you do want to tune it: leave it alone (the
50% default applies as soon as the quiet-window is set); set it explicitly to a percentage of
your own choosing; or set it to `0` to go back to firing on the quiet window alone, with no
context check at all. This floor only ever governs orrerix's own unprompted timing — **calling
`request_compact()` yourself always fires immediately**, at any context level, because that's
your judgment call, not orrerix's.

**The orchestrator can also ask for it directly.** `request_compact()` is the primary
mechanism — the timed nudge above is the fallback for personas that never call it. The
orchestrator (or any agent) calls it as the LAST action of a turn, at a natural lull; orrerix
pastes `/compact` the moment the pane actually goes idle, not immediately (a mid-turn write
would land as a queued message). Before calling it, the persona is expected to offload
durable state (task board, `set_state`, relevant GitHub issues/PRs) — the tool warns, but
never blocks, if that looks skipped. If a group sets a context-usage threshold (percent of
the model's context window), crossing it delivers an `[orrerix] context at NN% …` notice; if
the agent still hasn't asked by the next check, orrerix requests one on its behalf rather than
letting the CLI hit its own emergency auto-compact with no offload.

**Orrerix also catches that emergency auto-compact itself, when it happens anyway.** There's
no way to plan around a compact nobody asked for, but orrerix recognizes Claude Code's own
auto-compact banner in the pane and treats it the same as any other compact: whichever way
one gets triggered — the timed nudge, a direct request, the threshold fallback, a human
typing `/compact` by hand, or the CLI's own emergency auto-compact — once it's done, orrerix
re-grounds the pane in its full role instructions (not just a pointer to go re-read them)
and prompts it to re-sync live state. Before doing so, orrerix checks that context actually
shrank (a real signal a compaction ran, not just an ordinary quiet moment) — if it can't
confirm that, it skips the re-grounding rather than risk delivering it on a loop.

**Directive ledger.** Any agent can call `note_directive(text)` to jot down a one-line diary
entry — a human directive, a scope decision, a piece of feedback — the moment it receives
one, before acting on it. The point is timing: an emergency auto-compact strikes with no
warning turn, so there's no "offload before it happens" moment to rely on for something that
only ever lived in the conversation. Orrerix embeds each agent's own ledger (its recent tail,
size-capped, pointing at the full file if anything had to be cut) right alongside the role
instructions in that same re-grounding notice, so a directive survives a compact even when
nothing warned anyone first. `note_directive(text, replace: true)` rewrites the whole ledger
in one shot — how an agent curates it after being shown its own tail, dropping anything
already done or no longer relevant. The ledger lives at
`<data dir>/orrerix/orchestration/<group>/ledger-<agent-id>.log` — a plain, human-readable
file, one entry per line, that a human can open directly.

**Lifecycle panel.** The group lifecycle panel (`Alt+O`) shows each Claude agent's current
context-window usage (tokens + percent) next to its uptime and cost, and — only when there's
something worth a glance — its compact-nudge phase: armed (waiting to observe the pane go
busy), awaiting evidence (busy observed, waiting on quiet to resolve), re-grounding (a
reinjection is in flight, with its attempt count), a recently finished re-grounding (see
below), or a recent lost outcome (an arm or delivery that didn't resolve in time and was
released rather than left stuck). An idle agent
with nothing pending shows neither line. The percent is against the model's actual context
window, so a larger tier (Opus) reads correctly — a group can override the guess explicitly
if it's ever wrong for a given deployment.

**A finished re-grounding tells you how strong the evidence behind it was.** Orrerix stops
retrying a re-grounding on one of two signals, and they are not the same strength, so the
panel says which one it got rather than reporting both as success:

- **`re-grounding delivered`** — orrerix's own submit sampler watched the notice's Enter land.
  The text reached the pane's input box and was submitted.
- **`re-grounding unproven (agent alive)`** — no delivery confirmation ever arrived, but the
  agent called an orrerix tool afterwards. That proves the agent is alive and working; it
  proves nothing about the notice. A re-grounding that was genuinely lost, on a pane that
  happened to be busy for its own reasons, finishes exactly this way.

Neither one proves the agent *read* the re-grounding — nothing orrerix can observe from
outside an agent's session does, so it doesn't claim to. The audit log draws the same
distinction, under two separate actions (`compact-reinjection-confirmed` and
`compact-reinjection-liveness-only`), so counting one of them doesn't quietly include the
other. The safety net underneath both is unchanged: a re-grounding that neither confirms nor
draws any sign of life gets bounded retries and then a visible lost-outcome record.

## Persistence & restart

Each group keeps durable state under
`<data dir>/orrerix/orchestration/<group>/`:

- `state.json` — the orchestrator's queue/plan memory (written via a tool after
  every change);
- `audit.jsonl` — every tool call, prompt, spawn, and exit, one JSON line each;
- `agents.json` — the roster (which sessions belonged to which role);
- the rendered role instructions, plus the rendered **orchestrator playbook**
  (`orchestrator-playbook.md`) — the on-demand half of the orchestrator's contract, which
  the orchestrator reads one section at a time with `read_playbook(section)` (#1683);
- `ledger-<agent-id>.log` — each agent's own directive ledger (see **Compact-nudge** above).

The group id is derived from the repo path, so relaunching an orchestrator on the
same repo resumes its state; GitHub issues remain the source of truth for the
work queue.

**Restart after orrerix closes:** open the
[session browser](features/session-browser.html) (**`Ctrl+Shift+P`**). Its
**Orchestrations** section, at the top, lists every group orrerix has a record
of — on every agent CLI — and **Resume** there restores the *whole*
orchestration: same group id, state, task board, and audit history, with fresh
MCP identity wired into the resumed conversation. A plain `claude --resume` /
`copilot --resume` (or opencode's `--session <id>`) would come back powerless
(no MCP tools, no task board); this path never does.

That section reads orrerix's own record of the group (`group.json` plus the
orchestrator row of `agents.json`), not any CLI's session store — which is why
it is the route to use. The session list *below* it is a scan of each CLI's own
store, and an orchestration group's OpenCode sessions are deliberately not in
it: they live in the group's own store, never your global one (see
[Session browser](features/session-browser.html)). Orchestration sessions that
*are* in that list still carry `ORCH` / `W` / `REV` chips and still restore
their group when clicked — the route is decided by recorded membership, never
by which CLI wrote the session — and a worker/reviewer row rejoins its group
once the group is running.

A row that cannot be resumed says why, instead of offering a button that
fails:

| What the row says | What happened | What to do |
| --- | --- | --- |
| *Running now* | The group has live agents in this window | Focus its orchestrator pane |
| *Session not yet identified* | Copilot and OpenCode mint their session ids after boot, and orrerix has not learned this one yet (or its watcher timed out) | Wait for it. If the watcher timed out there is nothing to resume by hand — start a fresh orchestrator, which reattaches to this group's existing board and roster |
| *Recorded session is no longer in the … store* | The CLI's own history no longer holds that conversation | Start a fresh orchestrator — it reattaches to this group's existing board and roster |
| *This group's record could not be read* | The group's `group.json` is missing or damaged | Repair or remove that file; nothing can be resumed safely until orrerix can tell which CLI ran it |

**Per-task sessions:** each worker is scoped to exactly one work item, and orrerix
records its session id. Follow-ups on a finished task *resume* that worker's
session (same context, same workspace) instead of cold-starting a new agent or
disturbing a busy one.

**The delivery queue (above) persists to disk.** A restart doesn't drop a
queued prompt: an entry addressed to the group's orchestrator, or carrying the
same CLI session id as a pane that comes back, redelivers automatically in its
original order. Everything else is surfaced rather than replayed — the
orchestrator's session-start re-sync lists it with the payload intact where
one was recorded, so it can re-derive and re-send what still applies, rather
than the prompt silently vanishing. The one true loss is a delivery caught
mid-submit when orrerix went down: that text is not recoverable, and the
recovery notice says so plainly.
`doc/design/orchestration.md`'s "Delivery queue (#445)" section carries the
full design.

## Autonomous mode
{: #autonomous-mode }

Everything above describes the **supervised** default: the orchestrator advances
work in response to your nudges (**▶ Start**, the label handshake, steering) and
your merge-gate decisions, and no agent ever merges or publishes.

Two opt-in modes go further — an **autonomous** mode where the orchestrator wakes
itself on an idle timer and pulls labeled work while you're away (under a token
budget and optional auto-merge / auto-release consent toggles), and a
**supervised dangerous mode** that lets agents merge and release without per-item
approval while you're present. The default-branch merge/release gate that backs
them is structurally enforced, not just asked of the model.

**→ See [Autonomous & supervised modes](autonomous-mode.html)** for the full
picture: the idle tick, the cost/budget money-stop, each consent toggle and what
it gates, the per-item approve-with-comment grants, and the gate's audit trail.

## Requirements

- An agent CLI on `PATH` — `claude`, `copilot`, `gemini`, or `opencode`. Roles
  can run on different ones (see [cross-model reviewers](#setting-up-a-cross-model-reviewer)).
  The launcher warns inline as you pick, and re-checks on submit — if one of
  those CLIs isn't on `PATH` it refuses the whole launch rather than starting
  the group. A CLI named by a `.orrerix/workflow.yml` block is not checked at
  all, and shows up instead as a pane that opens and immediately exits with the
  shell's not-recognized error.
- `gh` CLI authenticated for the issue/PR/review workflow.
