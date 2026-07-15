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

**Permissions** are either *Auto* (Claude Code's native auto permission mode plus
pre-approved `git`/`gh` and loomux agent tools — recommended) or *Accept edits
only*. Loomux never uses `--dangerously-skip-permissions`.

Under *Auto*, **group Copilot** agents run in Copilot's true **autopilot mode**
(`--autopilot`) — an unattended worker should persist autonomously rather than
pause to ask — and loomux answers the resulting "Enable autopilot mode" consent
dialog for them automatically at spawn (your group-level *Auto* choice is the
consent). A lone single pane, where you're present to answer, does not enter
autopilot mode.

The launcher warns inline when any selected role's CLI isn't installed, and an
agent pane that dies with an error stays open so you can read what happened.

## How it works

Loomux hosts a local **MCP server**; every agent pane in a group connects with
its own identity token (`--strict-mcp-config`, so workers see nothing else). The
orchestrator:

- plans work as GitHub issues, labeling ones it owns **`agent-managed`**;
- decides worktree-vs-branch per task by mergeability — a worktree branch is cut
  from the repo's default branch (fetched fresh from origin), never from whatever
  the primary checkout happens to sit on, so parallel work starts from a clean
  base without a manual rebase;
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
  records them, and routes them back to a worker). Both land as a message in the
  orchestrator pane, exactly as if you'd typed it.
- **▶ Proceed** on a `prototype` item (a demo-gated deliverable awaiting your
  verdict) promotes it: two-click confirm flips it to `in-progress`, records
  your decision, and prompts the orchestrator to take the prototype to a full
  production build.
- **🗑 done (N)** deletes all `done` items in one action (two-click confirm).
- **🗑 selected (N)** deletes exactly the rows you tick, by id, in one action.

Items that only you can advance (`pr`, `human-testing`, `blocked`) are
highlighted so what's waiting on you stands out.

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
- **Audit viewer** (`Alt+A` or the history icon) — opens the group's
  `audit.jsonl` as a filterable, searchable timeline: every prompt, spawn, task
  edit, delivery outcome, and state write, one row each. A **follow** button
  live-tails new lines.

## CI watches (agent notifications)

Distinct from the 🔔 desktop-notification toggle above — that raises a toast for *you*; this
notice goes to the *agent*, typed into its own pane. Agents no longer sit watching a PR's CI:
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
stuck. Without it, a correctly-waiting agent and a genuinely hung one look identical until you
open the audit log; the internal watchdog nudge the orchestrator gets for a silent agent says
so too, when the silent agent holds a live watch. The audit viewer (`Alt+A`) has a one-line
sentence for each of a watch's six lifecycle events (registered, fired, expired, failed,
cancelled, cleaned up on agent exit) instead of raw JSON.

## Cross-workspace channels

Every orchestration group is isolated by design — one group's agents never see another's
context. Sometimes you want a narrow, explicit exception: two related repos open in
different tabs (a library and its consumer, a backend and its frontend), and you want one
agent to tell another "the API changed" or "I'm blocked on your PR" without relaying the
message through you. A **channel** is that exception, and it is opt-in every time: **only
you** can open, close, or redirect one. No agent can ever connect, join, disconnect, or
redirect a channel itself.

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
who's driving each one. The chip mirrors to a docked pane's minimized chip too, and a
background tab holding a connected pane gets its own small dot on the tab strip, so a
channel spanning a hidden tab is never invisible.

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
command), a claude/copilot pane launched with the checkbox off, or a pane you launched
before this feature existed, becomes connectable the first time you right-click it: it
joins as a **receive-only** member (a dashed variant of the chip, instead of solid) — it
can never be the sender, and its direction is always ▼. This is a structural fact, not a
bug: those CLIs have no way for loomux to hand them a channel-send capability today
(tracked as a follow-up), and an already-running pane can't be handed one either without
restarting it. A receive-only pane still gets every message the sender sends it — it just
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

**Limits (v1).** Channels are **in-memory only** — closing loomux drops every channel;
after a restart, reconnect the panes you want linked. A pane holds **at most one channel**
at a time (see Multi-party, above). Full (sender-capable) standalone membership only works
for claude/copilot today — see "Standalone panes" above.

## Group lifecycle

The orchestrator pane has a lifecycle toggle (`Alt+O` or the group icon) with a
one-glance summary — how many agents are live, the role breakdown, uptime, each
agent's state, and running session cost with a group total. From here you can:

- **Pause** the group — loomux stops delivering prompts so its agents finish
  their turn and idle out (reversible with resume).
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

## Guardrails

Enforced by loomux, not the model:

- a cap on live agents (≤12, set at launch and adjustable live);
- models pinned per role at launch;
- the permission mode fixed at group creation (native auto mode or acceptEdits —
  never bypass).

## Persistence & restart

Each group keeps durable state under
`<data dir>/loomux/orchestration/<group>/`:

- `state.json` — the orchestrator's queue/plan memory (written via a tool after
  every change);
- `audit.jsonl` — every tool call, prompt, spawn, and exit, one JSON line each;
- `agents.json` — the roster (which sessions belonged to which role);
- the rendered role instructions.

The group id is derived from the repo path, so relaunching an orchestrator on the
same repo resumes its state; GitHub issues remain the source of truth for the
work queue.

**Restart after loomux closes:** orchestration sessions are marked in the
[session browser](features/session-browser.html) (`ORCH` / `W` / `REV` chips).
Clicking a dead group's orchestrator session restores the *whole* orchestration
— same group id, state, task board, and audit history, with fresh MCP identity
wired into the resumed conversation. A plain `claude --resume` would come back
powerless (no MCP tools, no task board); this path never does.

**Per-task sessions:** each worker is scoped to exactly one work item, and loomux
records its session id. Follow-ups on a finished task *resume* that worker's
session (same context, same workspace) instead of cold-starting a new agent or
disturbing a busy one.

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

- `claude` CLI on `PATH`.
- `gh` CLI authenticated for the issue/PR/review workflow.
