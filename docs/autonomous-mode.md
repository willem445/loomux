---
title: Autonomous & supervised modes
nav_order: 5
---

# Autonomous & supervised modes
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

By default an orchestrator only acts when something pokes it — you type in its
pane, a worker reports, a task hits a merge gate, or you press **▶ Start**. It
never merges or publishes: agents open PRs and you gatekeep the merge.

Two opt-in modes change that, at opposite ends of the "am I here?" spectrum:

- **Autonomous mode** — *unattended.* The orchestrator wakes itself on an idle
  timer and keeps pulling **labeled** work off the board while you're away, under
  a token budget that hard-stops runaway spend. With the matching consent toggles
  it can also merge and even publish releases on its own.
- **Supervised dangerous mode** — *you're watching.* You stay in the loop but let
  agents merge to the default branch and cut releases without approving each one
  by hand.

Both are **off by default**, both survive an app restart, and both are
**mutually exclusive** — one is for when you've stepped away, the other for when
you're at the keyboard.

Two of the guarantees below are **structurally enforced by loomux** — an agent
that violates them is *blocked*, regardless of what it's instructed to do:

- the **merge / release gate** (a default-branch merge or a release/tag publish is
  refused unless a toggle or grant authorizes it);
- the **autonomous ↔ dangerous mutual exclusion** and the toggle dependencies (the
  backend rejects an invalid combination);
- the **token-budget money-stop** (crossing the cap suspends autonomous mode
  unconditionally).

Other behaviors on this page are **policy the orchestrator is *instructed* to
follow**, not a hard wall — they're delivered to it as prompt text, so they hold
as long as the orchestrator obeys its instructions, not as a boundary loomux
enforces. Each is flagged where it appears (the labeled-work-only intake, and the
"adequately tested" bar the orchestrator applies before self-merging). Treat those
as convention, the enforced items as guarantees.

## Where the controls live

All of these controls are in the **orchestrator group's lifecycle panel** (the
`Alt+O` / group-icon overlay on the orchestrator pane), in an **Autonomous mode**
section alongside pause, end-orchestration, and the max-agents stepper:

- an **Autonomous mode** on/off toggle;
- **Require human approval before merge** — a checkbox, *checked* by default
  (today's human merge gate). Unchecking it lets the orchestrator merge on its own
  while autonomous;
- **Auto-release** — a checkbox letting the orchestrator publish releases/tags
  itself while autonomous;
- **⚠ Dangerous mode** — a danger-styled toggle for supervised merges/releases
  while you're present (and *not* autonomous);
- a **Budget** input (tokens) with a live spend meter that appears while
  autonomous.

The controls grey out when they don't apply — auto-merge and auto-release are
locked off with a tooltip while autonomous is off; dangerous mode is locked off
with a tooltip while autonomous is on — so you're never offered a switch the
backend would reject.

## Autonomous mode

When you're away, nothing in loomux normally pokes an idle orchestrator, so its
periodic cadence (poll `agent-ready` / `agent-investigation`, groom, re-check open
PRs) simply never runs. Autonomous mode adds the missing **tick source**.

- **The idle tick.** A background timer watches each orchestrator pane. When it
  has been output-quiet — *and* free of your typing — for the idle-tick window, it
  gets exactly **one** `[loomux] idle tick` notice telling it to run its cadence
  and **start** labeled work. An orchestrator that's actually working (a real burst
  of output) resets the clock, so a busy group never gets nagged.
- **Labeled work only** *(policy, not enforced).* The idle-tick notice tells the
  orchestrator to start **labeled** issues (`agent-ready` / `agent-investigation`) —
  exactly the [label handshake](orchestration.html#the-label-handshake) you already
  control — and the orchestrator's instructions keep it to those. This is a
  convention the orchestrator is *instructed* to follow, not a gate loomux enforces:
  the label funnel is your consent boundary as long as the orchestrator obeys it,
  but nothing structurally blocks an unlabeled issue the way the merge gate blocks a
  merge. (Merging/publishing what it produces is still gated regardless.)
- **The window is tunable.** The default idle-tick window is **5 minutes**
  (adjustable per group, down to a minute or two if you want to watch it fire
  sooner). The autonomy panel shows a live countdown to the next eligible tick, and
  a hard per-hour cap backstops any pathological re-arming.
- **Pause still wins.** A [paused group](orchestration.html#group-lifecycle) is
  skipped entirely — no ticks, no deliveries — and your pause/off toggle is
  instant.

Autonomous mode is generic: loomux's own orchestration group is just another
group, so turning it on for the repo loomux itself is developed in would idle-tick
that orchestrator like any other.

## Cost guardrail: the token budget

Orchestration multiplies *unattended* spend, so autonomous mode ships with a hard
money-stop.

- **Set a budget.** The **Budget** field caps **autonomous-era** token spend.
  Leave it `0` (labeled *no cap*) for uncapped.
- **Metered from when you enabled it.** Turning autonomous on snapshots the group's
  current token total as an anchor; the meter counts spend **since that moment**,
  not lifetime history. The panel shows a live spend-vs-budget meter.
- **Crossing the cap suspends autonomy — unconditionally.** When autonomous-era
  spend reaches the budget, loomux **turns autonomous mode off**, delivers a single
  notice, and shows a distinct **"suspended: budget exhausted"** banner (separate
  from a plain toggle-off). This money-stop fires even if the underlying state file
  can't be written — continued spend past the cap is the one thing this feature
  must never allow — and the suspension survives a restart.
- **Re-enabling re-anchors.** To resume, you explicitly re-enable autonomous mode.
  That re-anchors the meter at the now-higher spend — which is what "toggle to
  resume" means. There is no way to dismiss the suspension *without* re-enabling.
- **Tokens, not dollars.** Subscription / Max accounts pay $0 marginal per token,
  so the budget is denominated in tokens, which is the honest, account-independent
  unit.

## The merge & release gate

"Never merge" used to be only an *instruction* in the orchestrator's prompt — and
a live incident proved instructions aren't a boundary (an orchestrator merged four
PRs straight to `main`). So the gate is now **structurally enforced**: every agent
pane runs behind a loomux `gh` / `git` shim that **blocks** a default-branch merge
or a release/tag publish unless it's authorized.

A default-branch **merge** or a **release/tag publish** is allowed only when one of
these holds:

| Path | Merge allowed when… | Release/tag allowed when… |
| --- | --- | --- |
| **Autonomous blanket** | `autonomous` **and** auto-merge on | `autonomous` **and** auto-release on |
| **Supervised** | dangerous mode on **and** *not* autonomous | dangerous mode on **and** *not* autonomous |
| **Per-item grant** | a valid grant for *that PR* | a valid grant for *that tag* |

Otherwise the action is **blocked** and the agent is told to report to you. What's
gated is: a merge onto the **default branch**; a `gh release create/edit/delete`
for **any** tag; and a `git push` that publishes a tag — an explicit `refs/tags/…`
or `git push … tag <name>` ref for **any** tag name, plus a bare tag-name push that
matches the release trigger pattern (`v*`, kept in sync with the release workflow)
and is confirmed a real tag. A merge onto a **non-default** branch (the
integration-branch flow agents use), a plain branch push, and read-only
`gh release view/list/download` all pass through untouched. An undeterminable merge
base **fails safe** (blocked).

> The shim is the cheap, always-on first layer — it raises an unattended bad merge
> from "type one command" to "deliberately evade a named control." A determined
> agent with shell access can still find bypasses (calling `gh`/`git` by absolute
> path, a raw REST call, forging a grant file under its own uid). The
> **authoritative** control is a **machine account**: give agents a GitHub token
> with no merge rights on the default branch and no tag-push rights (branch + tag
> protection). Then no client-side evasion matters — the server refuses. The two
> layers compose.

### Auto-merge (while autonomous)

Unchecking **Require human approval before merge** grants merge authority — but
**only in autonomous mode**. The dependency is enforced, not just implied:

- You can't enable auto-merge while autonomous is off (the checkbox is locked
  checked with a tooltip, and the backend rejects it).
- Turning autonomous **off** — or a budget suspension — **force-disables**
  auto-merge automatically.

When enabled, the orchestrator is *instructed* to merge only an **adequately-tested**
PR (reviewer-approved **+** green CI **+** acceptance met), audit and announce each
merge, and hold anything risky or ambiguous for you. That "adequately tested" bar is
**policy in the orchestrator's prompt, not something the gate checks** — once
auto-merge is on, the gate itself allows *any* default-branch merge and inspects no
CI or review state. So auto-merge is a delegation of judgment to the orchestrator,
not a guarantee that a red-CI PR will be refused; leave approval required if you want
that guarantee. Default: **off** (approval required).

### Auto-release (while autonomous)

Releases publish to the world — a `v*` tag push triggers the release workflow
(GitHub release + npm) — a bigger blast radius than a merge. So auto-release is a
**separate, independent** toggle:

- It's independent of auto-merge — you can allow self-merging while keeping
  releases manual, opt into both, or neither.
- Same autonomous dependency: enable only while autonomous; force-disabled when
  autonomous turns off or the budget suspends.
- Default **off**, so turning autonomous on **never surprise-publishes** — cutting
  a release stays a deliberate opt-in.

When on, the orchestrator may run `gh release create/edit/delete` and push a `v*`
tag itself; read-only `gh release view/list/download` was never gated.

## Supervised dangerous mode

Sometimes you're right there and just want to say "go ahead and merge / release"
without flipping into unattended autonomous mode. **Dangerous mode** is that: it
authorizes default-branch merges and release/tag publishes **without approving
each one**, while you supervise.

- **Only while *not* autonomous.** Dangerous mode is the supervised counterpart to
  autonomous, and the two are **mutually exclusive**, enforced both ways:

  | You do… | …and loomux |
  | --- | --- |
  | Enable dangerous mode while autonomous is on | **rejects it** with a clear error |
  | Enable autonomous while dangerous mode is on | **force-clears** dangerous mode (with a notice) |

- **Standalone and durable.** Unlike auto-merge/auto-release, dangerous mode is
  valid on its own (it *is* the not-autonomous posture) and survives a restart.
- **No auto-expiry (yet).** Dangerous mode is a standing switch with no TTL — you
  turn it off (or it clears when you enable autonomous). A time-based auto-expire is
  a noted future hardening, deliberately left out for now.
- **Default off**, and — like the grant setters — it can be set **only from the UI**,
  never by any agent tool.

## Per-item grants (approve without a blanket toggle)

The blanket toggles are all-or-nothing. When you want to approve **one** merge or
release without turning on auto-merge/auto-release, use a **grant** — the
approve-with-comment path:

- Clicking board **✓ Approve** on a `pr` / `human-testing` item (or the release-grant
  control for a tag) writes a one-time authorization for **that specific PR or tag**
  and tells the orchestrator to go ahead. You can attach a comment ("approved — bump
  the changelog first") delivered alongside the authorization.
- A grant is **single-use** (consumed the moment it's used) and **expires after 30
  minutes**. A grant for PR #5 can't authorize merging #7, and a merge grant can't
  authorize a release.
- Grants are written **only by these human surfaces** (board Approve and the
  grant commands) — **no agent tool can mint one.** Agents *consume* a grant through
  the shim; they never create one through loomux.

This is why simply clicking **Approve** works even with every blanket toggle off:
Approve writes the grant.

## The audit trail

Every gate decision — allow *and* block — is appended to the group's
`audit.jsonl`, and the **path** that authorized (or refused) an action is recorded
distinctly:

| Audit marker | Meaning |
| --- | --- |
| `merge-gate-allowed` / `release-gate-allowed` | the autonomous blanket toggle |
| `merge-gate-granted` / `release-gate-granted` | a one-time human grant |
| `merge-gate-dangerous` / `release-gate-dangerous` | supervised dangerous mode |
| `merge-gate-blocked` / `release-gate-blocked` | refused — logged with the reason (agent exits non-zero) |

So the trail always says *which* gate let something through, or why it was stopped.
Open it in the [audit viewer](orchestration.html#steering-attention-and-audit)
(`Alt+A`) — every merge, release, refusal, tick, and toggle change is one filterable
row.

## At a glance

| Control | Default | Active when | Authorizes |
| --- | --- | --- | --- |
| **Autonomous mode** | off | you're away | the idle tick that starts labeled work |
| **Token budget** | no cap | autonomous | hard-stops autonomous-era spend, then suspends |
| **Auto-merge** | off (approval required) | autonomous | orchestrator may self-merge default-branch PRs (instructed to require adequate testing) |
| **Auto-release** | off | autonomous | orchestrator publishes releases/tags |
| **Dangerous mode** | off | supervised (*not* autonomous) | manual merges/releases without per-item approval |
| **Per-item grant** | — | any time | one merge or one release, single-use, 30-min TTL |

## Requirements

- `gh` CLI authenticated (the gate resolves PR base branches and repo defaults
  through it).
- A group with a repository — the gate applies to default-branch merges, `gh
  release` commands, and tag pushes.
