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

Three of the guarantees below are **structurally enforced by loomux** — an agent
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
enforces. Each is flagged where it appears (the labeled-work-only intake, the
`agent-hold` veto under [full autonomy](#full-autonomy-the-orchestrator-picks-the-work),
and the "adequately tested" bar the orchestrator applies before self-merging).
Treat those as convention, the enforced items as guarantees.

## Where the controls live

All of these controls are in the **orchestrator group's lifecycle panel** (the
`Alt+O` / group-icon overlay on the orchestrator pane), in an **Autonomous mode**
section alongside pause, end-orchestration, and the max-agents stepper:

- an **Autonomous mode** on/off toggle;
- **Require human approval before merge** — a checkbox, *checked* by default.
  Unchecking it lets the orchestrator merge on its own while autonomous;
- **Auto-release** — a checkbox letting the orchestrator publish releases/tags
  itself while autonomous;
- **⚡ Full autonomy** — a checkbox (with a **Goal** field beside it) letting the
  orchestrator pick its own work instead of waiting for labels, while autonomous;
- **⚠ Dangerous mode** — a danger-styled toggle for supervised merges/releases
  while you're present (and *not* autonomous);
- a **Budget** input (tokens) with a live spend meter that appears while
  autonomous.

The controls grey out when they don't apply — auto-merge, auto-release and full
autonomy are locked off with a tooltip while autonomous is off; dangerous mode is
locked off with a tooltip while autonomous is on — so you're never offered a
switch the backend would reject.

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
  merge. (Merging/publishing what it produces is still gated regardless.) This is the
  default; [full autonomy](#full-autonomy-the-orchestrator-picks-the-work) inverts it
  for a group that opts in.
- **The window is tunable.** The default idle-tick window is **5 minutes**
  (adjustable per group, down to a minute or two if you want to watch it fire
  sooner). The autonomy panel shows a live countdown to the next eligible tick, and
  a hard per-hour cap backstops any pathological re-arming.
- **Your typing can only defer the tick so long.** Recent typing in the pane defers
  the tick — it never fires while you're actively steering — but that deferral is
  capped at 15 minutes past the orchestrator's last real output (`group.json`'s
  `idle_tick_input_defer_max_minutes`; no panel control yet, hand-edit the file to
  change it). The cap matters because terminal traffic a pane generates on its own
  (a Copilot orchestrator is the usual case) can read as "you're typing" with
  nobody there, and an uncapped deferral would then silence the tick until you
  pressed Enter yourself. Past the bound the tick fires anyway — a genuinely
  quiet stretch (no output, no typing) is either a stuck pane or you've stepped
  away, and both are correct to check in on.
- **Pause still wins.** A [paused group](orchestration.html#group-lifecycle) is
  skipped entirely — no ticks, no deliveries — and your pause/off toggle is
  instant.
- **An intake gate runs by default whenever autonomous mode is on.** Out of the
  box, the idle window firing is not sufficient on its own: loomux also
  runs a zero-token, host-side check (`gh issue list` / `gh pr list`, no LLM
  turn) for the same signals the tick exists to catch — a new
  `agent-ready`/`agent-investigation` label, an open PR's checks going green or
  red, and a new comment or review on an open PR — and the orchestrator is
  only actually woken when there's something new — or when something else
  needs it (an outstanding CI watch, an unresolved stalled-worker notice). A
  tick with nothing to report is skipped quietly (visible in the audit trail
  as `idle-tick-skipped`, with a `"suppressed":true` marker), and a bounded
  fallback (every few hours, tunable, and never disableable) still wakes the
  orchestrator regardless, so a quiet stretch is never left unchecked forever.
  **To opt a group OUT of the gate** (autonomous ticks then fire unconditionally
  on the idle window alone) — set `intake_poll_minutes`
  to `0` by hand in that group's `group.json` (there's no panel control for
  this; it's a deliberate, explicit override, not a toggle). Any other value
  there sets a custom poll cadence in minutes instead of the default — a floor,
  not a schedule: a scan polls at most a few groups, so with many autonomous
  groups coming due together, some are picked up by the next scan a minute
  later rather than all in one burst.
- **A group parked on you gets checked on less and less often.** That bounded
  fallback is the one wake nothing can suppress, so a group where everything is
  waiting on *you* — every open item human-gated, no delegates running, nothing
  moving on GitHub — used to pay it at full price indefinitely, and over a
  parked weekend that adds up to dozens of wake-ups that find nothing. So the
  fallback now backs off while nothing changes: each unconditional wake that
  finds nothing doubles the wait before the next one, up to a ceiling of a day
  (**3h → 6h → 12h → 24h** with the defaults). It snaps straight back to the
  base interval the moment anything happens — a label, a PR's checks, a comment
  or review, you typing in the pane, or any delegate being alive in the group.
  The backstop is coarser, never absent: a fully parked group is still woken
  unconditionally at least once a day. Both ends are per-group and hand-edited
  in `group.json` (`idle_tick_fallback_minutes` and
  `idle_tick_fallback_max_minutes`); setting the two equal turns the backoff off
  and restores a fixed cadence. The audit trail records the decay — each
  `idle-tick` entry carries the `empty_streak` it reached and the
  `fallback_minutes` interval that produced it.

Autonomous mode is generic: loomux's own orchestration group is just another
group, so turning it on for the repo loomux itself is developed in would idle-tick
that orchestrator like any other.

## Full autonomy: the orchestrator picks the work

Autonomous mode keeps the **label funnel**: the orchestrator wakes itself, but it
still starts only what you labeled `agent-ready` / `agent-investigation`. Full
autonomy inverts that default for the group — on each idle tick the orchestrator
selects the highest-value **eligible** open issue and starts it, until nothing
eligible is left. It's a sub-mode of autonomous (the checkbox is locked off while
autonomous is off, and turning autonomous off turns this off too).

- **What "eligible" means.** Open, **not** labeled `agent-hold`, and not already
  tracked by a board task. Everything else is fair game — that's the inversion.
- **The goal.** The optional **Goal** field is one line describing what this run
  is *for* ("harden any bugs, close out new issues identified as you work"). It
  travels with the enable and is echoed into the orchestrator's kickoff config and
  its toggle notice. **loomux never interprets it** — ranking candidates against
  the goal, and stating a one-line rationale per pickup, is the orchestrator's
  job, not policy in loomux. It's normalized to a single bounded line (whitespace
  collapsed, 500 characters) because it's typed into a CLI pane. Edit it while the
  mode is on and you **re-aim** the run: the orchestrator is re-notified and
  re-triages.
- **Set the goal, then enable.** There's no separate "save goal" action — the
  enable is what carries it, exactly like the budget field.

### The hold label is your veto

`agent-hold` marks an issue **agents must not start**. In the issues view
(`Alt+I`) it's a third toggle beside ready/investigate, rendered as a red **hold**
chip; `gh issue edit <n> --add-label agent-hold` does the same thing. The label is
created on demand with the description *"Held by the human — full-autonomy agents
must not start this"*, so it reads correctly on GitHub for anyone who's never seen
loomux.

**If your repo renamed it** (`intake.labels.hold:` in `.loomux/workflow.yml`),
that spelling is the veto everywhere: the issues-view toggle writes it, the
backend's label allow-list permits it and creates it on demand, and the
orchestrator's own contract names it — so the hand-typed `gh` command above is
`--add-label <your spelling>` too. A veto only some layers can see is not a veto,
which is why the rename reaches all of them rather than the poller alone.

- **loomux's half is host-side and zero-token:** a held issue is excluded from the
  eligible-work signal the intake poll produces, so the orchestrator is never even
  woken about it. Matching is case-insensitive, and a repo that renamed the label
  has *its* spelling honored — a veto that silently didn't match would be the one
  failure this must not have.
- **The rest is policy** *(not enforced)*: the orchestrator's instructions make the
  label absolute — it may never remove it, argue with it, or start under it — but
  nothing structurally blocks a start the way the merge gate blocks a merge. It may
  *add* the label to issues it files itself.

### Turning it on: triage, then veto

Enabling doesn't hand over the backlog; it starts a conversation about the backlog.

1. **You flip ⚡ Full autonomy** (optionally with a goal). The orchestrator gets a
   notice stating the protocol and — deliberately — that nothing about merging,
   releasing, review or budgets changed.
2. **It posts one ranked triage plan** as a GitHub issue: every open issue, one row
   each, with value / risk / effort / proposed order, and each row naming the veto
   gesture.
3. **You strike rows by labeling them `agent-hold`** (or your repo's own hold
   spelling) — one click per row in the issues view. There's no separate veto
   mechanism to learn, and nothing parses your edits to the plan; the label *is*
   the strike.
4. **You type "go" in the orchestrator's pane.** Until then the pre-existing
   backlog doesn't start. If you never say go, it never starts — that's a correct
   outcome, not a stall, and there's no timer that proceeds without you.
5. **From then on it self-selects**, announcing each pickup with a one-line
   rationale in its pane and on the board task. Issues filed *after* the enable
   don't wait for another triage round: if one fits the goal, it's eligible when
   it appears.

Priority order for what it picks: your board order first, then a milestone or
priority label, then `agent-ready`, then its own stated judgment against the goal.
An eligible issue that doesn't fit the goal is **parked** at the bottom of the
board with a note rather than started — it comes back at the next triage or when
you re-aim the goal.

### What full autonomy does NOT change

This toggle widens what may be **started**, never what may be **shipped**:

- the **merge gate** and **release gate** are untouched — a default-branch merge or
  a release still needs auto-merge / auto-release / a per-item grant, and the shim
  still enforces that regardless of this toggle;
- **review discipline** is unchanged: every PR still goes through review and the
  orchestrator's findings-disposition rules;
- the **token budget** still meters and still money-stops — and because full
  autonomy is the mode that *starts* work, a budget suspension force-clears it
  along with autonomous mode (you'll see both notices);
- the **agent cap** and spawn-rate limits are unchanged.

Turning it off restores the opt-in funnel immediately (the orchestrator is
notified, and finishes what's already in flight normally). The setting is durable
across restarts, but only *with* autonomous mode: a full-autonomy marker found
without a live autonomous one is cleared and audited on startup rather than
resumed, so the inverted default can never come back on consent nobody renewed.
Enables, re-aims and every disable — including the forced ones — land in
`audit.jsonl` as `full-autonomy-on` / `full-autonomy-goal-set` / `full-autonomy-off`
(with the goal, and with the reason for a forced clear).

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

"Never merge" is **structurally enforced**, not merely instructed — an instruction
in a prompt is not a boundary. Every agent pane runs behind a loomux `gh` / `git`
shim that **blocks** a default-branch merge or a release/tag publish unless it's
authorized.

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
tag itself; read-only `gh release view/list/download` is not gated.

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
  turn it off yourself, or it clears when you enable autonomous. A time-based
  auto-expire is a planned hardening, so don't rely on this staying manual forever.
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
- A **merge** grant is **single-use** (consumed the moment it's used) and **expires
  after 30 minutes**. A grant for PR #5 can't authorize merging #7, and a merge grant
  can't authorize a release.
- A **release** grant covers the whole release of **one tag** — pushing that tag,
  creating or editing its GitHub release, and writing that release's notes — and
  **expires after 90 minutes**. It is not single-use, because a release is not one
  command: the tag push kicks off a build that takes tens of minutes, and the notes
  are written against the release that build created. It authorizes **no other tag
  and no other release** (a call that names a release by numeric id is resolved back
  to its tag before it is checked, and refused if that tag isn't the granted one or
  can't be resolved at all), nothing at all once it expires, and **not** the version
  bump PR's merge — that still needs its own Approve.
- **Approving several at once** (board **✓ Approve selected (N)**) is the same thing
  N times, not something wider: one separate single-use, 30-minute grant per PR, and
  no "bulk" authorization exists for the shim to honour. The only difference is that
  the orchestrator hears about the batch **once** — one message listing every approved
  PR and any per-item notes — instead of once per PR.
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
| `merge-gate-granted` / `release-gate-granted` | an explicit human grant |
| `merge-gate-dangerous` / `release-gate-dangerous` | supervised dangerous mode |
| `merge-gate-blocked` / `release-gate-blocked` | refused — logged with the reason (agent exits non-zero) |
| `release-id-resolved` / `release-id-unresolved` | a call naming a release by numeric id was (or could not be) resolved to its tag before the grant was checked |

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
| **Full autonomy** | off (label funnel) | autonomous | orchestrator self-selects any open issue except `agent-hold`ed ones — starting only, never shipping |
| **Dangerous mode** | off | supervised (*not* autonomous) | manual merges/releases without per-item approval |
| **Per-item grant** | — | any time | one merge (single-use, 30-min TTL) or one tag's whole release (90-min TTL) |

## Requirements

- `gh` CLI authenticated (the gate resolves PR base branches and repo defaults
  through it).
- A group with a repository — the gate applies to default-branch merges, `gh
  release` commands, and tag pushes.
