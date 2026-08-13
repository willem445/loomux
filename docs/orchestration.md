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

Loomux's headline feature is a native **orchestrator / worker** pattern: a
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
   role; override any role you like. Model dropdowns are populated by querying
   the selected CLI's own help, so new models appear automatically, with a
   custom-entry escape hatch.
3. Set the repository, how many idle workers to start with, and the guardrails:
   **max live agents** and **permissions**.

### Thinking level and context window

Beside each role's CLI and model select, the launcher offers two more
per-role knobs: **thinking level** and **context window** — three knobs in
total (model, thinking level, context) for each of orchestrator, worker,
reviewer, and planner. All three default to **CLI default** — the empty
value, which loomux emits nothing for, so the CLI runs exactly as it would
with no flag at all.

- **Thinking level** sets how hard the model reasons before answering
  (`low`/`medium`/`high`/`xhigh`/`max`), on Claude Code only. Copilot CLI and
  Gemini CLI grey the control out with a reason shown inline: Copilot's
  effort level lives in `~/.copilot/settings.json` with no flag or
  environment variable to set it, and loomux never writes a user's global
  settings file to reach it; Gemini's thinking level is a settings-file key
  too (`modelConfigs.aliases.<alias>.thinkingConfig`) — the seam loomux uses
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
  suffix to). A model id loomux doesn't recognize — a full model name it
  hasn't seen, or a Bedrock/Vertex/Foundry deployment name — leaves the knob
  **enabled** instead: loomux only disables what it can affirmatively rule
  out, never what it merely doesn't know, since on those providers the
  suffix is exactly how the 1M window gets selected.

A knob that clears both checks is still an entitlement, not a guarantee:
`opus[1m]` is a real, documented alias, but `[1m]` access is plan- and
credit-gated on Claude's side, so picking it for an account that can't serve
it fails visibly at the CLI, in the pane — loomux doesn't pre-judge your
account's entitlements by hiding the option. That's a different failure from
the model gate above: the model gate hides a suffix that has no defined
meaning at all, while the entitlement case leaves a meaningful suffix
selectable and lets the vendor's own check decide.

These same two keys are available per block in `.loomux/workflow.yml`
(`effort:`/`context:`) for the advanced orchestrator. Loading the file
enforces the closed vocabulary and the per-CLI rule above; the workflow pane
goes further and also validates `context:` against the block's `model:`,
raising a per-block finding when the two disagree (e.g. `model: haiku` with
`context: 1m`) — the same model-gate rule the launcher's select uses, so a
hand-edited file can't drift from what the launcher would show. See
[`doc/design/workflows.md`](https://github.com/willem445/loomux/blob/main/doc/design/workflows.md)
and the `author-loomux-workflow` skill.

**Permissions** are either *Auto* (Claude Code's native auto permission mode plus
pre-approved `git`/`gh` and loomux agent tools — recommended) or *Accept edits
only*. Loomux never uses `--dangerously-skip-permissions`.

Under *Auto*, **group Copilot** agents run in Copilot's true **autopilot mode**
(`--autopilot`) — an unattended worker should persist autonomously rather than
pause to ask — and loomux answers the resulting "Enable autopilot mode" consent
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
  `.loomux/workflow.yml` **that validates**, and runs that roster instead of the
  built-in four roles. If the file is there but broken, you're told so and
  there's no checkbox: a new group runs the built-in roles, the same outcome the
  launcher warns about inline. Either way — valid file, broken file or no file —
  a **reattached dormant group keeps the roster its own launch approved**; which
  roster a promotion runs depends on the group case as much as on the file.

The item is offered on standalone **Claude** agent panes. It's greyed with the
reason on an agent pane that can't be promoted *yet* — a non-Claude CLI, a pane
loomux hasn't learned a session id for (send it a prompt first: an agent nobody
has spoken to has no conversation to carry over), or a pane with no working
directory to make the group's repo — and it isn't offered at all on a shell pane,
a pane running something that isn't an agent CLI, or a pane that already belongs
to an orchestration group.
loomux also refuses a session that was *ever* a recorded member of a group, even
a long-dormant one: a delegate's transcript carries a delegate's contract, and
two role contracts in one session is not a thing to seat on purpose. Every
refusal says which one it is, and a refused promotion changes nothing at all —
the pane keeps running exactly as it was.

If the relaunch itself fails after the old process was stopped (a spawn that
doesn't come up, a bind that times out), loomux does **not** quietly start a new
session in its place — that would look like success while discarding the very
thing you promoted. The group is already durable on disk, so the toast names it
and points you at its **Resume** card in the session browser, which brings the
conversation back.

Promotion is human-only, like every other gesture on this menu: no agent can
promote a pane, its own or anyone else's.

## How it works

Loomux hosts a local **MCP server**; every agent pane in a group connects with
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
its plan through it), the loomux tools, and — so it can ground a plan in a
vendor's actual reference docs rather than in recall — `WebFetch`/`WebSearch`.
That last pair means a planner pane can reach arbitrary hosts, which is worth
knowing if you plan on sensitive repositories; to switch it off, add a `WebFetch`
(or `WebSearch`) entry to `permissions.deny` in the repository's own
`.claude/settings.json` — a deny rule there beats anything loomux pre-approves.
Running a build or test command is *not* pre-approved, and loomux offers no way to
widen that — a planner's persona `allow:` patterns are dropped, unconditionally.
Your own `.claude/settings.json` can still add one if you decide to: permission
rules merge across scopes rather than override — the same merge rule that makes
the `WebFetch` switch-off above work. So what loomux *denies* there is no way to
allow, but what it merely leaves out of its allow-list — general `Bash`, and so
`cargo check` — a repo-level `permissions.allow` can grant. Absent that, a plan
will say when it could not confirm something by running it.

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

You can apply `agent-ready` / `agent-investigation` straight from the
[GitHub issues view](features/github-issues.html) — toggle the **ready** or
**investigate** control on an issue row. If the repo doesn't have these labels
yet, loomux creates the one you toggle on first use (only these allow-listed
labels are ever created).

## The task board

The orchestrator pane has a board toggle (`Alt+T` or the list icon) showing the
group's work queue — status per item, issue/PR links, notes, and priority order.
You can add, edit, annotate, reorder, and delete tasks; the orchestrator is
notified of your edits and maintains the same board through its tools. Issue and
PR chips are **clickable** and open in your browser.

Statuses: `queued`, `in-progress`, `review`, `pr`, `human-testing`,
`prototype`, `done`, `blocked`.

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
    loomux could not resolve the repo's default branch) — the warning above.
    Unknown is treated as "assume the default branch": a board that guessed the
    other way would quietly downplay a merge straight into `main`.
  - **Base is some other branch** — a stacked sub-PR into an integration
    branch, say — the label says so and names it ("Approve (sub-PR into
    integration/581 — the orchestrator merges it once the gate verdicts land)").
    Your Approve grant is the *default-branch* gate, so it is not what this
    PR is waiting on.

  This narrows the **story**, not the gate. A custom workflow's merge gate
  applies to every merge of a PR wherever it lands, integration branches
  included, and it is enforced against the base ref loomux resolves live at
  merge time — never against what a task says. The recorded base is display
  metadata the orchestrator writes, so treat it the way you'd treat any other
  board text: informative, not authoritative. Two ways the label can be
  *wrong-but-harmless*, both worth knowing: the orchestrator can record a stale
  base if it retargets the PR without updating the board, and loomux reads your
  repo's default branch from the clone's own refs rather than fetching, so a
  default branch renamed on the remote reads as the old name until something
  fetches. Either way the worst case is a sentence that misdescribes the PR —
  no merge is authorized by any of this.
- **▶ Proceed** on a `prototype` item (a demo-gated deliverable awaiting your
  verdict) promotes it: two-click confirm flips it to `in-progress`, records
  your decision, and prompts the orchestrator to take the prototype to a full
  production build.
- **🗑 done (N)** deletes all `done` items in one action (two-click confirm).
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
  gets a bold, glowing, gently pulsing treatment and a **"● ACTIVE — \<agent
  id\>"** badge — the first thing your eye should land on. This is deliberately
  the loudest state on the board.
- **Idle** — the status still says `in-progress`/`review`, but the assignee
  isn't a currently-live agent (its pane was killed, or it's an older session).
  The row reads as muted, not active — an idle/stalled assignment can never be
  confused with real live work.

The assignee chip itself carries the same distinction: a **live** agent gets its
own blue tint, while a **history** chip (an assignee that isn't currently live)
reads dimmed and in italics — so an old assignee on a done, reopened, or stalled
task never looks like the same agent is still sitting there. `done` items dim
further still, receding behind whatever's still active.

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
  **▸ ready**, next to ▶ Start. The ready mark appears only once some task on
  the board actually declares a dependency — on a board that uses none, every
  queued item is trivially ready and the badge would say nothing.

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

## Steering, attention, and audit

These deserve their own detail — see:

- **[Steering & attachments](features/steering.html)** — the collision-proof compose
  strip under an orchestrator pane (`Alt+P`), and pasting screenshots into a
  message.
- **Attention routing** — a pane earns a pulsing **needs-attention** chip when an
  agent is parked on a prompt only you can answer, when a worker reports done or
  blocked, or when a task hits a human merge gate. An optional per-group
  **desktop notification** toggle (🔔 in the lifecycle panel) raises an OS toast
  for those events (off by default).
  - Most chips clear themselves: they're recomputed every few seconds, and
    clicking one focuses the pane and acknowledges it.
  - The red **⚠ stuck prompt** chip is the exception. It means a prompt loomux
    sent was never submitted, and it stays up until loomux has evidence the pane
    is fine again. loomux keeps looking for that evidence for as long as the chip
    is up — every half-minute or so it re-reads the pane, even long after the
    delivery that raised the chip has been given up on. If the prompt has left the
    box **and** you have typed in that pane since, the chip comes down on its own;
    if the prompt is still sitting there, or loomux cannot read enough of the pane
    to tell, the chip stays, because taking it down would be the one mistake it
    exists to prevent.
  - Some chips have no reading that could ever release them — one reporting
    messages that were discarded while the group was paused is about something
    that already happened. So that chip carries its own **✕ dismiss** button
    beside it. Clicking ✕ takes the chip down for good, whatever raised it.
  - **✕ takes the chip down and nothing else.** It does not unstick the pane, and
    loomux never presses Enter on your behalf because you dismissed something. If
    the prompt really is still sitting unsubmitted in that pane's input box, it
    still is afterwards — so if you're not sure, look at the pane first (hover the
    chip: it says what it thinks is blocking). Every dismissal is written to the
    group's audit log with what was dismissed and how long it had been up, so a
    chip you cleared can always be looked up later.
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
  human's own line still sitting in its input box — loomux holds a prompt
  delivery rather than typing over it. A hold that never clears **queues** the
  prompt instead of dropping it: nothing is lost, there's no timeout (you might
  be away for hours), and the queue drains automatically, in order, the moment
  the pane is free again — the first thing it delivers is a one-line summary of
  what was waiting. One exception to "no timeout", and it only ever moves a queue
  *forward*: if the hold is the interactive-question one, and after 15 minutes the
  pane's own screen still shows its ordinary input box, loomux stops believing its
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
  is the one case loomux can't announce this way — a prompt about that pane's
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
  them would read as "these are all still missing". Loomux never re-sends them
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
a specific GitHub Actions run — and go do other work; loomux polls in the background (every
30s) and types a `[loomux] PR #241 checks: SUCCESS — … (watch n-3)`-style notice into the
registering agent's own pane the moment it resolves, expires, or fails repeatedly. A watch is
capped (4 per agent / 12 per group) and time-bounded (5–240 min, default 60). Pausing a group
freezes a watch entirely (no polling, firing, or expiry) until you resume it.

Watches live only in memory, and the two ways an agent loses track of one are different:
a `/compact` drops the agent's *memory* of a watch (the watch itself is still registered and
still live), so it re-lists to recover what it was waiting on; closing loomux drops the watch
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

**When a notice can't get in.** A `[loomux]` notice is typed into the agent's pane, and a pane
that is mid-turn can't take one — so if an agent blocks its own turn waiting on the very thing
the notice would tell it about, the notice sits in that pane's queue and nothing clears. Loomux
watches for exactly that shape: a pane that has accepted nothing for ten minutes while holding
one of loomux's own notices gets a `[loomux] notice undeliverable 10 min: … — pane mid-turn …`
sent to the group's orchestrator (and to the audit viewer as `notice-undeliverable`), alongside
the ⏸ held chip and the needs-attention badge you'd see anyway. The message says which of the
three it looked like — a pane mid-turn, a human's own line in the box, or a dialog waiting for an
answer — because those need different things from you. If the stuck pane is the orchestrator's
own, the notice rides back on its next tool call instead of being typed into it.

**When you fix a stuck prompt yourself.** A **⚠ stuck prompt** chip means a prompt was typed
into that pane but never submitted, and loomux queues a repair — one Enter, held back until the
pane is safe to write to. If you get there first (click into the pane and press Enter, or clear
the box), loomux now checks the pane rather than the keyboard: once the prompt is no longer
sitting in the box, the repair is dropped instead of pressed a second time, the chip comes down,
and anything queued behind it — steering messages, worker reports — starts flowing again on the
next poll. Before this, your fix was invisible to the queue, and continuing to type in that pane
kept the repair pending indefinitely, silently holding everything behind it.

The repair is only ever dropped once loomux can *see* that the prompt has left the box. While
it is still sitting there, loomux keeps waiting for a safe moment to press Enter — because the
alternative is pasting the next message on top of it and sending the two merged into one. So a
pane blocked by your own half-typed line, or by a dialog waiting for an answer, still holds its
queue, and the chip is what tells you it needs you.

**And on a pane nobody is delivering to any more.** The check above rides a repair or a
delivery, so it used to stop once loomux gave up on the pane — after which a chip on a
finished worker or a drained orchestrator could stay up until you restarted. It no longer
does: for as long as a **⚠ stuck prompt** chip is up, loomux re-reads that pane every half
minute and applies the same rule. Two things have to be true together for the chip to come
down by itself — the prompt is no longer in the box, **and** you have typed in that pane
since loomux submitted it. Either alone is not enough, deliberately: a prompt that vanished
with nobody at the keyboard is exactly the case where the chip is the only trace left of it,
and a keystroke on its own says nothing about where the prompt went.

Between those, the wording can still change under you. A chip that said loomux could not
read the pane, or that its repair attempts were used up, becomes "its text is gone — check
the pane" once loomux can see the box is clear. That is the chip getting more honest, not a
new problem: it stays up, and it stays up until you deal with it or dismiss it. Whatever
takes a chip down — your ✕, or loomux's own reading — is written to the group's audit log
with what it was and what was read, so you can always look up where one went.

**One chip is answered by loomux retrying, not by loomux looking.** A **⚠ stuck prompt** chip
that says the repair could not even be *queued* — the pane already had the maximum number of
messages waiting — is not a claim about what is in the box, so re-reading the pane could never
settle it. Instead, once that pane's queue has drained and nothing is delivering to it, loomux
queues the repair it could not queue before, and the chip changes to "loomux is re-sending it".
From there it behaves like any other repair: held back until the pane is safe to write to,
dropped if you get there first, and gone once the message lands. If the queue is still full, or
a delivery is in progress, the chip stays exactly as it was and loomux tries again shortly —
nothing is taken down on a guess. Before this the repair was simply never attempted again, so a
pane that filled up once could carry that chip until you restarted, with the prompt still
sitting unsent in its box.

## Lock resources (taking turns on something scarce)

Some things on your machine only one agent can use at a time — a compile that saturates
every core, a GPU, a device on a USB port, a fixed port number, a staging database. Without
help, four workers will reach for it at once. **Lock resources** let you declare those things
once, in your repo, and have agents take turns.

Declare them in `.loomux/workflow.yml` (the same file that carries your roster, so this needs
the **advanced orchestrator** on):

```yaml
resources:
  build:
    slots: 1              # how many agents may hold it at once — 1 is a mutex
    max_hold_minutes: 45  # loomux takes it back after this, whatever happens
  gpu:
    slots: 2              # omitting max_hold_minutes means the default, 30
```

What a resource *means* is entirely yours — loomux never learns that `build` is a compiler.
It knows the name, the slot count and the clock. Declare nothing and the feature is off:
the agents in that group are not even offered the tools.

**Both settings are optional, and the defaults are not what the example above shows.** Omit
`slots` and you get **1**; omit `max_hold_minutes` and you get **30**, not the 45 written out in
the example. A name may use letters, digits, `-` and `_` (up to 48 characters) — anything else is
rejected rather than quietly rewritten, so the name you type is the name your agents call.

**A value out of range fails the whole file, on purpose.** `slots: 0` or above 64,
`max_hold_minutes: 0` or above 480, a name with an illegal character, an unrecognized key inside a
resource, or more than 32 resources are all **hard errors that stop `.loomux/workflow.yml` from
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
`[loomux] lock 'build' is yours` notice typed into its pane when its turn comes, exactly like
a CI watch resolving. That is not a convenience: a notice is *typed into a pane*, and a pane
that sat blocking on its own lock could never receive the one telling it to proceed.

**The queue is first-come, first-served**, and every wait is bounded: a queued request gives
up after `wait_minutes` (default 60, 5–240) and the agent is told so, rather than waiting on
a notice that will never arrive. An agent that changes its mind can call `release_lock` while
queued to withdraw, so nobody sits behind a slot it no longer wants.

**A lock always comes back.** Three things can end a hold: the holder releases it, the holder's
pane dies (loomux reclaims it immediately and hands it to whoever is next), or the hold runs
past `max_hold_minutes` (loomux reclaims it and tells the ex-holder its work is no longer
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

Lock state lives in memory only. Closing loomux clears it, which is the right answer: every
pane that could have been holding a lock died with it.

**It is cooperative, and deliberately so.** A lock is taken because an agent asked for one —
loomux does not intercept your build command, and an agent that never calls `acquire_lock` is
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
time — it lands as a typed `[loomux] channel chan-N - <name> (<role>, <repo>): <text>`
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
is a structural fact, not a bug: those CLIs have no way for loomux to hand them a
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

**Limits.** Channels are **in-memory only** — closing loomux drops every channel;
after a restart, reconnect the panes you want linked. A pane holds **at most one channel**
at a time (see Multi-party, above). Full (sender-capable) standalone membership only works
for claude/copilot today — see "Standalone panes" above.

## Group lifecycle

The orchestrator pane has a lifecycle toggle (`Alt+O` or the group icon) with a
one-glance summary — how many agents are live, the role breakdown, uptime, each
agent's state, and running session cost with a group total.

**Where those cost figures come from.** Tokens are exact — read from each CLI's own
record of the session, never scraped off the pane. Dollars depend on the CLI. For
Claude Code, loomux prices the tokens itself against a dated table, so the figure is
an **estimate** — and on a subscription/Max account the real marginal cost is $0
regardless, which is why tokens are the honest metric there. OpenCode prices its own
sessions, so loomux **reports** its number instead of guessing one (including a
genuine $0.00 on a free model, which is an answer, not a blank). Each total is
labelled accordingly — *estimated*, *reported*, or *mixed* for a group running both.
A CLI with no readable record falls back to whatever dollar figure it prints in its
own statusline, which disappears when the pane does.

From the lifecycle panel you can:

- **Pause** the group — loomux stops delivering prompts so its agents finish
  their turn and idle out (reversible with resume). **Pausing holds deliveries
  rather than dropping them**: nothing is typed into a pane while you're
  paused, so nobody spends tokens, but a worker's `done` report fired
  mid-pause is queued — on disk, so it survives a restart taken during the
  pause — and delivered when you resume, labelled as having waited on the
  pause rather than on a blocked pane. An agent *spawned* during a pause is
  held the same way, and resumes as the boot it is: loomux still waits for
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
- **Max live agents** stepper (1–12) — adjust the cap on the fly; loomux
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
  immediately, and the orchestrator's pane gets a `[loomux] workflow mode
  changed: …` notice so it can adjust its spawn/review strategy mid-session.
  Agents already running keep the role they were spawned under; only new
  spawns pick up the swapped roster.

## Custom agent workflows

By default a group runs the built-in four-role roster — one orchestrator,
worker, reviewer, and planner, each on the CLI/model you picked at launch. A
repo can commit `<repo>/.loomux/workflow.yml` and declare its own instead: any
number of named blocks, each with its own capability class (orchestrator,
worker, reviewer, or planner), CLI, model, and persona, plus a **merge gate**
naming which reviewer blocks must record a `pass` verdict — enforced
mechanically by the `gh` shim — before `gh pr merge` can succeed. See
[`doc/design/workflows.md`](https://github.com/willem445/loomux/blob/main/doc/design/workflows.md)
for the full design.

**If your repo squash-merges, consider `also: [body-unchanged]`.** A verdict is
bound to the commit it reviewed, so a re-push re-opens the gate. The PR *body*
is not part of that commit — and a squash merge turns it into the permanent
commit message, so a body edited after a reviewer passed lands text nobody
reviewed. loomux always records a digest of the body a verdict reviewed and
tells the orchestrator when it has moved (on a `pass`: the approval no longer
covers what would be committed; on a `fail`: the finding may already be fixed).
Adding `body-unchanged` to your gate's `also:` list also *refuses the merge*
until the reviewers whose passes are live have re-recorded against the body as
it stands. It is opt-in because it is only true of squash-merging repos; where
merges keep the PR body as discussion rather than history, leave it out.

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

**loomux never silently arms a gate it can't satisfy.** If a workflow's merge
gate names reviewer blocks the currently-running roster can't actually spawn —
most commonly a broken or missing `workflow.yml` on a relaunch — loomux
doesn't drop the gate to keep merges flowing; it arms the gate anyway and
shows a loud warning in the lifecycle panel, so the mismatch is something you
see rather than something a bounced merge makes you go find.

**Copilot personas: a `tools:` list must let loomux through.** If a block sets
`cli: copilot` and points `profile:` at a `.github/agents/*.md` file, Copilot
loads that file directly — and if the file declares a `tools:` list, that list
*filters* everything the agent can reach, MCP servers included. A list that
never names loomux produces a delegate that can see the loomux server and use
none of it: it cannot report, read the task board, or be steered, and from
inside its own pane it looks like loomux is broken. Add the server to the list:

```yaml
tools: ["read", "edit", "execute", "loomux/*"]
```

loomux repairs this for you where it safely can — it launches such a block from
its own stand-in for the persona, carrying every line of your file plus that
grant, and never edits anything in your repo — but it always tells you, in the
spawn reply and the audit log, which file to fix.

Three cases it deliberately leaves alone, because each is a choice you made
rather than something you forgot. It says so, and the fix is yours:

- the persona declares its own `mcp-servers:` — loomux's stand-in would drop them;
- `tools: []`, which disables every tool — loomux won't turn "nothing" into
  "nothing except loomux";
- the list already names loomux per-tool (`loomux/report`) — loomux won't widen a
  scope you set on purpose.

**Reviewer diversity across models.** A block's `cli`/`model` are set
per-block, so nothing stops a reviewer lane from running on a different
CLI/model than the one that wrote the code — a second model tends to catch a
different class of defect than the one already primed on its own output.
Worth considering for any reviewer-heavy workflow; loomux's own dogfood
`.loomux/workflow.yml` notes the same above its reviewer blocks.

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
- **Adding the block is not inert to older loomux builds.** The workflow file rejects keys it
  does not recognize, so on a build that predates the merge queue, `merge_queue:` fails the
  parse of the **whole file** — your `gates:` included — rather than being ignored. That is
  deliberate (a key the build doesn't understand means you believe a policy is in force that
  isn't), but it means everyone sharing the repo wants to be on a build that has it.

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

You need the CLI itself installed and logged in — loomux spawns `gemini` from
your `PATH` the same way it spawns `claude`. A CLI named by a workflow block is
**not** checked before launch (only the CLIs picked in the launcher's own role
dropdowns are), so if it isn't installed the pane still opens, prints the
shell's not-recognized error (on Windows, `The term 'gemini' is not
recognized…`), and exits; the orchestrator is then told that agent died. Nothing else is needed: the reviewer's loomux
tools (including the `pass`/`fail` verdict the merge gate reads) are wired up
per agent, and its containment is generated per agent too, so a gemini
reviewer is denied the file-editing tools exactly like a Claude one.

Two differences worth knowing before you pick gemini for a lane:

- **`allow:` doesn't apply to a gemini block.** Those patterns are
  Claude/Copilot tool-matcher strings. A gemini block runs with its class's
  baseline and can't be widened.
- **No compact nudge.** loomux's context-pressure nudge types `/compact`,
  which gemini doesn't have (its command is `/compress`), so gemini agents
  are skipped rather than sent a command that doesn't exist.
- **No session history features.** loomux can't resume a gemini session or
  read its transcript — gemini mints its own session ids rather than
  accepting one, so there's nothing for loomux to record and reopen later.

`cli: opencode` is a fourth choice, and it runs opposite gemini on that last
point:

- **`allow:` doesn't apply to an opencode block either.** Its permission keys
  (`edit`, `bash`, …) are a different namespace from the Claude/Copilot
  tool-matcher strings `allow:` speaks, so an opencode block also runs with
  its class's baseline and can't be widened — same decision, same reason, as
  gemini's arm.
- **No compact nudge.** opencode isn't on the short list of CLIs loomux will
  paste `/compact` into (claude and copilot only), so an opencode agent's
  context management is left to the CLI itself, same as gemini's.
- **Session history *does* work.** opencode has no `--session-id` to hand a
  pane up front, so loomux learns which session is the reviewer's after it
  starts rather than minting one — but once bound, that session resumes and
  its transcript reads back like any claude or copilot one.

**Why not codex?** codex can't deny its editing tool by name, and its sandbox
is all-or-nothing — strict enough to block the tests and `gh` a review needs,
or open enough to let the reviewer rewrite the code it's reviewing. A reviewer
that can't be contained would quietly weaken the merge gate, so loomux refuses
the pairing rather than shipping it.

Turning it on live shows the same resolved-roster confirm (name, blocks, any
declared gate) the launcher's own preview shows at launch time; turning it off
confirms that future spawns fall back to the built-in roster on your default
CLI (per-role CLI overrides picked at launch aren't separately retained, so an
off→on→off round trip rebuilds the roster from your default CLI rather than
restoring them).

### Proposed lessons come with their evidence

A workflow can declare a **process-pro** block — a worker that runs after a
PR merges, reads that session's record cold, and opens a normal PR proposing
a durable lesson (an entry in `.loomux/lessons.md`, a `.claude/skills/` entry,
a `CLAUDE.md` rule). Like every other agent it proposes and stops: you review
and merge, or you don't.

The thing worth knowing when one of those PRs lands in front of you is what
it is allowed to claim. Anything the process-pro writes into those files is
inlined into every future session's context, so a wrong or trivial lesson is
a cost you keep paying — which makes "was this actually a recurring problem,
or did one agent have one bad afternoon?" the question the review turns on.

loomux answers it mechanically rather than leaving it to the agent's opinion
of itself. Each piece of friction it found carries a **recurrence** count:
how many *other* sessions in the group hit the same wall, and which ones.
So a proposal should read like *"three sessions hit this — `w-2`, `w-7`,
`w-9`"*, and you can go look at those sessions. A proposal from a wall only
one session ever hit is supposed to say so and argue why it will recur anyway
(a documented rule somebody missed, say) — if it doesn't, that is your cue to
push back rather than merge a lesson built on one bad afternoon.

Two caveats the proposal should carry when they apply, because they change
what the number is worth: a brand-new group has no earlier sessions to
compare against, so a `0` there means *nothing to compare*, not *never
happened*; and only a bounded number of recent sessions are read, so on a
long-running group a count is a floor rather than a total.

### What actually reaches a kickoff from `.loomux/lessons.md`

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

A repo can turn on a **merge queue** (`merge_queue:` in its `.loomux/workflow.yml`) so a
batch of approved sub-PRs is tested *together* on a scratch ref before any of them reaches
the integration branch — the combination is what gets a gate, instead of each PR getting one
and nobody checking the pile. The queue runs in loomux itself and lands only on an
integration branch, never on your default branch; see
[`doc/design/merge-queue.md`](https://github.com/willem445/loomux/blob/main/doc/design/merge-queue.md)
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
loomux cannot read the queue's state file at all — a torn write, or a file written by a newer
build — it says **that**, loudly, instead of drawing an empty queue: "nothing is queued" and
"loomux can't read the queue" are the same picture otherwise, and only one of them means
your PR is fine.

**How quickly it moves.** The queue is driven by loomux's background poller, which wakes
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
comment lands on that PR with the failing check and the batch's sibling set. loomux
deliberately does **not** brief the PR's author itself: attribution is mechanical, but
deciding who picks it up — and whether to resume that worker or spawn a fresh one — is a
judgment call, so it stays the orchestrator's.

Two honest limits, both of which the culprit comment states in as many words. Bisect isolates
**a** culprit, not necessarily **the** culprit: when two changes are each fine alone and only
fail together, the search blames whichever one the split isolated, and the comment names the
siblings so you can see that rather than being told a half-truth confidently. And a batch that
comes back **unverifiable** implicates no PR at all — the checks never resolved, nothing
landed, and the thing to look at is your CI.

## Guardrails

Enforced by loomux, not the model:

- a cap on live agents (≤12, set at launch and adjustable live);
- models pinned per role at launch;
- the permission mode fixed at group creation (native auto mode or acceptEdits —
  never bypass).

### Compact-nudge

The orchestrator pane lives for the whole session and every turn re-reads its entire
history — it's typically the biggest token consumer in a group. Loomux can drive Claude
Code's own `/compact` for it at a natural lull: once an eligible pane has been idle at its
input prompt (the same output-quiet signal the watchdog and idle-tick already read — never
mid-turn) past a configured window, loomux pastes `/compact` for it exactly like any other
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
context check at all. This floor only ever governs loomux's own unprompted timing — **calling
`request_compact()` yourself always fires immediately**, at any context level, because that's
your judgment call, not loomux's.

**The orchestrator can also ask for it directly.** `request_compact()` is the primary
mechanism — the timed nudge above is the fallback for personas that never call it. The
orchestrator (or any agent) calls it as the LAST action of a turn, at a natural lull; loomux
pastes `/compact` the moment the pane actually goes idle, not immediately (a mid-turn write
would land as a queued message). Before calling it, the persona is expected to offload
durable state (task board, `set_state`, relevant GitHub issues/PRs) — the tool warns, but
never blocks, if that looks skipped. If a group sets a context-usage threshold (percent of
the model's context window), crossing it delivers a `[loomux] context at NN% …` notice; if
the agent still hasn't asked by the next check, loomux requests one on its behalf rather than
letting the CLI hit its own emergency auto-compact with no offload.

**Loomux also catches that emergency auto-compact itself, when it happens anyway.** There's
no way to plan around a compact nobody asked for, but loomux recognizes Claude Code's own
auto-compact banner in the pane and treats it the same as any other compact: whichever way
one gets triggered — the timed nudge, a direct request, the threshold fallback, a human
typing `/compact` by hand, or the CLI's own emergency auto-compact — once it's done, loomux
re-grounds the pane in its full role instructions (not just a pointer to go re-read them)
and prompts it to re-sync live state. Before doing so, loomux checks that context actually
shrank (a real signal a compaction ran, not just an ordinary quiet moment) — if it can't
confirm that, it skips the re-grounding rather than risk delivering it on a loop.

**Directive ledger.** Any agent can call `note_directive(text)` to jot down a one-line diary
entry — a human directive, a scope decision, a piece of feedback — the moment it receives
one, before acting on it. The point is timing: an emergency auto-compact strikes with no
warning turn, so there's no "offload before it happens" moment to rely on for something that
only ever lived in the conversation. Loomux embeds each agent's own ledger (its recent tail,
size-capped, pointing at the full file if anything had to be cut) right alongside the role
instructions in that same re-grounding notice, so a directive survives a compact even when
nothing warned anyone first. `note_directive(text, replace: true)` rewrites the whole ledger
in one shot — how an agent curates it after being shown its own tail, dropping anything
already done or no longer relevant. The ledger lives at
`<data dir>/loomux/orchestration/<group>/ledger-<agent-id>.log` — a plain, human-readable
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

**A finished re-grounding tells you how strong the evidence behind it was.** Loomux stops
retrying a re-grounding on one of two signals, and they are not the same strength, so the
panel says which one it got rather than reporting both as success:

- **`re-grounding delivered`** — loomux's own submit sampler watched the notice's Enter land.
  The text reached the pane's input box and was submitted.
- **`re-grounding unproven (agent alive)`** — no delivery confirmation ever arrived, but the
  agent called a loomux tool afterwards. That proves the agent is alive and working; it
  proves nothing about the notice. A re-grounding that was genuinely lost, on a pane that
  happened to be busy for its own reasons, finishes exactly this way.

Neither one proves the agent *read* the re-grounding — nothing loomux can observe from
outside an agent's session does, so it doesn't claim to. The audit log draws the same
distinction, under two separate actions (`compact-reinjection-confirmed` and
`compact-reinjection-liveness-only`), so counting one of them doesn't quietly include the
other. The safety net underneath both is unchanged: a re-grounding that neither confirms nor
draws any sign of life gets bounded retries and then a visible lost-outcome record.

## Persistence & restart

Each group keeps durable state under
`<data dir>/loomux/orchestration/<group>/`:

- `state.json` — the orchestrator's queue/plan memory (written via a tool after
  every change);
- `audit.jsonl` — every tool call, prompt, spawn, and exit, one JSON line each;
- `agents.json` — the roster (which sessions belonged to which role);
- the rendered role instructions;
- `ledger-<agent-id>.log` — each agent's own directive ledger (see **Compact-nudge** above).

The group id is derived from the repo path, so relaunching an orchestrator on the
same repo resumes its state; GitHub issues remain the source of truth for the
work queue.

**Restart after loomux closes:** orchestration sessions are marked in the
[session browser](features/session-browser.html) (`ORCH` / `W` / `REV` chips).
Clicking a dead group's orchestrator session restores the *whole* orchestration
— same group id, state, task board, and audit history, with fresh MCP identity
wired into the resumed conversation — whether that orchestrator runs Claude
Code, Copilot CLI, or OpenCode. A plain `claude --resume` / `copilot --resume`
(or opencode's `--session <id>`) would come back powerless (no MCP tools, no
task board); this path never does. Whether a clicked row takes it is decided
by the recorded membership the chip itself reflects, never by which CLI wrote
the session — a chipped row restores its group on every agent CLI, and a row
with no chip is a plain session and restores as one.

**Per-task sessions:** each worker is scoped to exactly one work item, and loomux
records its session id. Follow-ups on a finished task *resume* that worker's
session (same context, same workspace) instead of cold-starting a new agent or
disturbing a busy one.

**The delivery queue (above) is in-memory only.** If loomux restarts while a
prompt is queued behind a blocked pane, that queued prompt is lost — every
enqueue is still recorded in `audit.jsonl`, so the loss is visible after the
fact, but nothing replays it automatically **yet** — a replay is a planned
follow-up, and `doc/design/orchestration.md`'s "Delivery queue (#445)" section
carries the argument for why it isn't there today.

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
  the group. A CLI named by a `.loomux/workflow.yml` block is not checked at
  all, and shows up instead as a pane that opens and immediately exits with the
  shell's not-recognized error.
- `gh` CLI authenticated for the issue/PR/review workflow.
