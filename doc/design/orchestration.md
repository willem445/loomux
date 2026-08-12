# Design: native orchestrator / worker agent orchestration

Status: implemented (feat/orchestration). Builds on `doc/plans/mcp-orchestration-backend.md`,
extended with roles, guardrails, git-workflow automation, persistence, and audit.

## Problem

A single agent per repo can't absorb a queue of upcoming work without burning its own
context window. The user wants to hand ideas (or GitHub issues) to a long-lived
**orchestrator** agent that plans, schedules, and delegates to **worker** agents — each in
its own visible loomux pane — with a separate **reviewer** agent per PR, while the human
only gatekeeps final review + merge.

## Principles

1. **Panes, not subagents.** Every agent is a normal `claude` CLI in its own pane so the
   human can watch and steer any of them directly.
2. **Visible prompts.** All inter-agent communication is delivered by *typing into the
   recipient's CLI* (bracketed paste + Enter). What the orchestrator tells a worker looks
   exactly like a user prompt, is steerable, and is captured in the audit log.
3. **Guardrails in the platform, judgment in the prompt.** Loomux enforces hard limits
   (max live agents, pinned worker/reviewer models, group isolation); the orchestrator's
   scheduling judgment (worktree vs branch, serial vs parallel by mergeability) lives in
   its instruction template.
4. **Nothing merges without the human.** Agents open PRs; only the user merges.
5. **Survive restarts.** Claude Code isn't a 24/7 daemon. Durable state = GitHub issues
   (labeled `agent-managed`) + a per-group `state.json` the orchestrator reads/writes via
   MCP tools. Relaunching an orchestrator on the same repo reattaches to that state.

## Architecture

```
┌────────────────────────── loomux (Tauri) ──────────────────────────┐
│  Rust backend                                                      │
│   ┌ OrchRegistry ─ groups, agents, roles, tokens, guardrails       │
│   │   state dir: <data>/loomux/orchestration/<group>/              │
│   │     group.json  state.json  audit.jsonl  configs/<agent>.json  │
│   ├ MCP server (tiny_http, 127.0.0.1:ephemeral)                    │
│   │   identity: X-Loomux-Agent token header → (group, agent, role) │
│   └ PtyManager ─ ring buffer tee (get_output), prompt injection    │
│  Frontend                                                          │
│   orchestration.ts ─ listens orch-spawn-request → opens badged     │
│   pane → bind_agent(agent_id, pty_id); group colors; focus         │
└────────────────────────────────────────────────────────────────────┘
        ▲ MCP over HTTP (per-agent token)         │ typed prompts (PTY stdin)
   claude CLIs: orchestrator (opus) · workers (pinned model) · reviewers
```

- **Spawn round-trip** (panes are frontend-owned): MCP `spawn_agent` → registry mints
  agent + token + mcp-config → emits `orch-spawn-request` → frontend opens pane, reports
  `bind_agent(agent_id, pty_id)` → registry unblocks the tool call (mpsc, 20s timeout)
  → kickoff prompt typed into the new pane after a boot delay.
- **Spawn expiry / cancellation (#106):** the round-trip has no in-band ack, so a
  frontend stalled past the 20s bind wait used to service the request late — opening a
  *zombie pane* whose CLI booted against a config the timeout had already deleted, plus
  an unhandled `no pending bind` toast. Three layers now prevent it: (1) each
  `orch-spawn-request` carries a `deadline_ms` (`now + BIND_TIMEOUT`); the frontend drops
  any request already past it (`spawn_request_expired`, mirrored in `spawnexpiry.ts`) with
  a console breadcrumb and no toast. (2) On bind timeout the backend emits
  `orch-spawn-cancelled`, so a live-but-slow frontend drops the queued request (and closes
  any pane already opened for it). (3) A late `bind_agent` still errors ("no pending
  bind"); the frontend now handles that rejection by closing the just-opened pane (killing
  the stray CLI) with a brief "stale spawn request discarded" toast — belt-and-braces for
  the ordering where a pane opens before the cancel arrives.
- **Registry hygiene (#106):** `list_agents` keeps a dead agent's identity
  (id/name/role/session/status/cwd — needed to resume its session) but drops its task
  body; dead records accumulate across a run and the full briefs had pushed one group's
  roster payload to ~86KB.
- **Isolation:** tools only see the caller's group. Panes without a token (normal shells,
  unrelated agents) have no access at all. `--strict-mcp-config` keeps workers off the
  user's other MCP servers.
- **Completion signals:** workers call `report(status, summary)` → loomux types
  `[loomux] <name> reports …` into the orchestrator pane (queued if mid-turn) + audits it.
  PTY exit marks the agent dead and notifies the orchestrator the same way.

### Pane process model: direct-CLI spawn (#78)

Each pane is a ConPTY (`OpenConsole.exe` host) plus its child process tree. The agent
CLI (`claude`/`copilot`) **is** the child — spawned directly, no wrapper shell:

```
loomux.exe
├─ OpenConsole.exe … (ConPTY host, 1 per pane — inherent)
└─ claude.exe --session-id … --mcp-config … (the agent — inherent)
```

Earlier every agent pane wrapped the CLI in a shell — `OpenConsole → pwsh -Command "claude …"
→ claude.exe` — because `claude`/`copilot` used to ship as `.cmd`/`.ps1` PATH shims that only
a shell could resolve. They are native `.exe` now, so the wrapper was pure overhead: one extra
process + ~40–70 MB per pane, ~⅓ of a group's process count, and an extra layer where kills,
typed input, and env could go sideways.

`spawn_agent` now emits **both** a shell `command` string (the historical form) and a
structured `argv` (program + literal args, built by `build_agent_argv` from the same flag
atoms as `build_agent_command`; a test tokenizes the string and asserts it equals the argv, so
the two can't drift). `spawn_pty` resolves `argv[0]` on PATH+PATHEXT (the shared
`winpath::resolve_program`, reused from "open in editor") and, when it is a **native**
executable (`winpath::is_native_executable`: `.exe`/`.com`, not a `.cmd`/`.ps1` shim),
`CommandBuilder`s it directly as the ConPTY child. It falls back to wrapping `command` in the
shell — the exact pre-#78 behavior — when resolution fails, the target is a shim, the escape
hatch `LOOMUX_NO_DIRECT_SPAWN` is set (any value but empty/`0`/`false`), **or the resolved native
exe fails to actually spawn** (corrupt/truncated PE, AV/ACL block, arch mismatch — caught in
`spawn_pane_child` so a bad exe degrades to the wrapper instead of dying at the #106 bind
timeout). Every fallback is breadcrumbed (`pty-direct` / `pty-direct-fallback`).

Steady-state process count for a typical group (1 orchestrator + 3 workers + 1 reviewer):

| | wrapper (pre-#78) | direct-CLI spawn |
| --- | --- | --- |
| ConPTY hosts (`OpenConsole.exe`) | 5 | 5 |
| wrapper shells (`pwsh.exe`) | 5 | **0** |
| agent CLIs (`claude`/`copilot`) | 5 | 5 |
| **total** | **15** | **10** (−33%) |

Scope: only the orchestration agent panes (known native CLIs) direct-spawn. Plain shell panes
and the launcher's custom-command panes keep the shell — that's their purpose — as do shim CLIs
(`gemini`/`opencode` installs that ship a `.cmd`), which the native-vs-shim check routes back to
the wrapper automatically. OSC 7 cwd reporting is unaffected: agent panes never used the
interactive shell's `cd`-reporting hook (they show no prompt); their branch/cwd chip is seeded
statically from the spawn directory. Pane teardown is unchanged and *improved* — the kill-on-close
Job Object (see [job-object-teardown.md](job-object-teardown.md)) now enrolls the agent itself
rather than a wrapper, and an agent exit surfaces the CLI's own exit code directly (no pwsh in
between), handled by the existing dead-pane path (expected kill → pane closes; unexpected exit →
pane stays open showing the status).

## Tool surface (MCP)

| tool | orchestrator | worker/reviewer/planner |
| --- | --- | --- |
| `spawn_agent(name, kind, task, worktree?, branch?, base?)` — `kind` (or `block`) is required on a fresh spawn, [#544](#a-capability-class-is-never-acquired-by-omission-544) | ✓ (guardrailed) | ✗ |
| `send_prompt(agent_id, text)` | ✓ | ✗ |
| `report(status?, summary?, outcome?, ref?, detail_url?, note?)` / `message_orchestrator(text)` | ✗ | ✓ |
| `list_agents()` | ✓ | ✓ |
| `get_output(agent_id, lines)` | ✓ | ✗ |
| `kill_agent(agent_id)` / `focus_agent(agent_id)` | ✓ | ✗ |
| `rename_agent(agent_id, name)` | ✓ | ✗ |
| `get_state()` | ✓ | ✓ |
| `set_state(state)` | ✓ | ✗ |
| `group_usage(detail?)` | ✓ | ✗ |
| `notify_when(kind, pr?, run?, note?, expires_minutes?)` | ✓ | worker/reviewer only (✗ planner) |
| `list_notifications()` | ✓ | worker/reviewer only (✗ planner) |
| `cancel_notification(id)` | ✓ | worker/reviewer only (✗ planner) |
| `channel_send(text)` | ✓ | orchestrator/worker/reviewer (✗ planner) |
| `channel_status()` | ✓ | orchestrator/worker/reviewer (✗ planner) |
| `session_digest(task? \| agent? \| pr?)` | ✗ | `process`-hinted worker blocks only (✗ plain worker, ✗ reviewer, ✗ planner) |

`session_digest` (#250/#324 slice B, gate tightened in slice D) reads a
session's transcript — Claude `.jsonl` or Copilot `session-state`, normalized
to one event shape — and reduces it, deterministically and without an LLM, to
friction windows (a failing tool call and its recovery, a near-duplicate
command re-run, a test red-to-green, a reverted edit) plus three anchors
(initial prompt, final diff/PR ref, task outcome). It never returns the raw
transcript. The target session need not still be alive: it is meant to be
read cold, after the worker that produced it is gone — see
`OrchRegistry::session_digest` in `orchestration/mod.rs` and
`orchestration/digest.rs`. Gated to `role_hint == process` worker blocks — the
process-pro's own tool, not a general worker one; slice B shipped this
worker-kind-wide as an interim, deliberately coarser exposure while
`role_hint` (slice A) was still landing in parallel, and slice D's binding
rider tightened it once role_hint was on the branch.

Each window also carries a normalized `key` and a **cross-session
`recurrence`** count (#324): how many OTHER sessions in the same group hit the
same wall, with up to five `corroborated_by` agent ids. That number is what
answers the process-pro's durability filter — "would a fresh worker hit this
too?" — mechanically instead of from the agent's own impression of one
session. The scan is derived on read (nothing cached, nothing persisted) over
at most `MAX_CORROBORATION_SESSIONS` other transcripts, and the digest reports
`sessions_scanned` and `corroboration_capped` so a young group reads
differently from a group of one-offs and a capped scan can't read as an
exhaustive one. Design and the ledger-vs-derived fork:
`doc/design/supervisor-skills.md`, "Cross-session recurrence".

Guardrails enforced by `spawn_agent`: live-agent cap (`max_agents`, counting workers +
reviewers + planners), CLI + model pinned per role (`{role}_cli` / `{role}_model`, see
**Plan agent + mixed agent types** below), permission mode fixed at group creation
(`acceptEdits` default; full-auto opt-in). Worktree creation reuses `git_worktree_add`
(never for a planner — it is read-only). `worktree` now defaults **on** for a worker spawn
and cannot be turned off (see **Worker worktree is mandatory** below).

### Worktree base branch (#204)

`git_worktree_add` cuts the agent branch from the repo's **default branch**, not the
primary checkout's `HEAD`. It fetches `origin` first, then branches from the remote's
advertised default (`origin/HEAD`, falling back to `origin/main`/`origin/master`); offline
it uses the local default branch and drops a `worktree-base` breadcrumb. The primary
checkout's `HEAD` is incidental state — before this fix, a worktree spawned while the main
copy sat on a feature branch inherited that branch's commits, so agent PRs shipped stray
commits and burned review rounds. The optional `base` arg overrides the start-point so an
orchestrator can deliberately stack a worktree on an in-flight feature branch instead of
instructing the worker to rebase by hand.

The branch is created and checked out in one `git worktree add --no-track -b <name> <dir>
<base>`. Keeping the `-b` and the start-point in a single command is deliberate: the naive
"fix" (`git worktree add <dir> <origin-ref>` then `git switch -c`) checks out a
remote-tracking ref, which lands the new worktree on a **detached HEAD** until the switch
runs — the transient-detached-HEAD incidents reported alongside #204. The pre-#204 code was
already a single `worktree add -b` off `HEAD` (attached, no detach window), so those
incidents were not reproducible from our code path in isolation — most plausibly git's
repo-level worktree lock racing under concurrent spawns; this fix keeps the atomic-`-b`
invariant so the fix itself cannot introduce a detached-HEAD window.

`kind` is `worker` (default), `reviewer`, or `planner`. A **planner** explores the
codebase read-only and writes a structured implementation plan as a GitHub issue comment,
then reports and exits; it never writes code, branches, worktrees, or PRs.

### Worker (and, since #359, reviewer) worktrees are mandatory (#338)

Before this, `worktree` defaulted to `false`: a worker spawned with no explicit `worktree:
true` worked directly in the group's primary clone — the same checkout the human uses. That
was fine as long as the orchestrator happened to choose a worktree for anything that could
collide with the human (in practice it almost always did), but "almost always" is prose, not
a guarantee, and the whole point of the primary clone is that it's the *human's* environment:
they may have it open in an editor, mid-rebase, or running the dev server, and a worker
`checkout`/commit/push landing there under them is a real, live conflict, not a hypothetical.

**The fix is mechanism, not a stronger recommendation in the templates.** `worktree` for a
worker now defaults to `true`, and — this is the part that had to be a deliberate choice,
not just a default flip — **passing `worktree: false` for a worker (or a worker-kind
`block`) is a hard error**, enforced in `mcp.rs`'s `spawn_agent` dispatch, not a silent
coercion to `true`. Two shapes were on the table:

- **Reject the explicit `false`** (chosen). Consistent with this file's own precedent
  (#222: an unrecognized `kind` is REJECTED, never silently coerced to `worker`) and with
  the repo-wide convention of failing loud on a request that contradicts a hard constraint,
  rather than quietly doing something else. An orchestrator that explicitly asks for
  `worktree: false` on a worker has a wrong mental model of the guarantee, and coercing it
  would hide that from the very system prompt (`spawn_agent`'s tool description, and
  `orchestrator.md`) that is supposed to teach it the guarantee exists.
- **Coerce + warn** (rejected). Cheaper for a caller that doesn't care, but it means the
  tool's return value carries a warning an LLM caller may not weight as strongly as an error,
  and it re-opens exactly the failure mode #338 exists to close: a caller believing it got
  what it asked for. A hard error is unambiguous in a way a coerced-and-logged success is not.

The guard reads the **effective role** (the named block's `kind` when one is given, falling
back to the `kind` argument otherwise — the same precedence `spawn_agent_ex` itself applies),
not just the `kind` argument, so a worker-kind `block` is covered exactly like the bare
`kind: "worker"` default; naming an *unknown* block is left to `spawn_agent_ex`'s own "unknown
block" error rather than pre-empted by this guard (`needs_dedicated_workspace` in `mcp.rs` is
the one place that decides which roles this covers — originally just `Role::Worker`, extended
to `Role::Reviewer` by #359 below). A planner is untouched — it never gets a worktree under
any `worktree` value, per its existing read-only contract.

**Guarding `worktree` alone left two more doors into the main clone open, both found on review
of the #338 PR itself — `cwd` bypasses the flag entirely, on either entry point:**

- **A fresh spawn's explicit `cwd` (a follow-up review finding on the same PR).** `spawn_agent_ex`'s `cwd_override`
  branch wins over `worktree` unconditionally — that's what makes `cwd` useful for a resume,
  but it means a plain `spawn_agent(kind: "worker", cwd: "<anywhere>")`, no `resume_session` at
  all, bypassed the worktree guard completely: `worktree`'s own value never even matters once
  `cwd` is set. The tool description had called `cwd` "ignored without resume_session" — true
  of nothing in the code, just unenforced prose. Fixed by rejecting an explicit `cwd` on a
  fresh worker (now: worker-or-reviewer) spawn or block, same style and the same `#338` wording
  as an explicit `worktree: false` — checked *before* the `worktree: false` check even runs,
  since an explicit `cwd` makes that check moot regardless of what `worktree` says. A planner is
  unaffected: a fresh spawn's `cwd` is still honored for it as a raw override, unchanged.
- **A resume's omitted `cwd` (rev-13's finding).** `cwd` is documented as "required with
  resume_session", but nothing enforced that either — a resume with `resume_session` set and
  `cwd` omitted fell straight through into `spawn_agent_ex`'s per-role default (`cwd_override`
  is `None`, and `worktree` itself defaults `false` for a resume), which for a worker (or,
  since #359, a reviewer) is the primary clone. Fixed by mirroring #254's own block inheritance,
  deliberately, rather than inventing a second mechanism: a resume that omits `cwd` now inherits
  the session's recorded workspace from this group's roster (the same last-touched-record
  lookup, `owner` in `mcp.rs`, shared with the block-inheritance code so the two agree on which
  record is authoritative instead of running independent lookups that could drift). If nothing
  is recorded for that session and the effective role needs a dedicated workspace, the spawn is
  rejected — same style and the same `#338` guardrail wording again — rather than guessing a
  workspace or falling back to the clone. A planner is unaffected here too: an omitted `cwd`
  with nothing recorded still falls through to its existing per-role default, unchanged.

Between the three guards, `cwd` and `worktree` together can no longer land a worker or reviewer
in the primary clone on either a fresh spawn or a resume, however the two arguments are combined.

**The orchestrator's own mechanical work** (a rebase, a conflict fix, cutting a revert branch)
still sometimes needs a checkout outside a worker's own worktree — and now that a worker
worktree is guaranteed, doing that work in the primary clone would recreate the exact conflict
this issue closes, just from the orchestrator's side instead of a worker's. There's no new
tool for this (the ask was "keep it minimal"): `orchestrator.md`'s **Re-sync the fleet**
section now documents the convention directly — reuse the PR's own worker worktree if it's
still around, otherwise cut a `git worktree add <repo>-worktrees/orch-staging <branch>`
staging worktree (same `<repo>-worktrees/` layout `git_worktree_add` already uses for
workers) and reuse that one directory across mechanical work by checking out a different
branch inside it, rather than a fresh worktree per rebase.

### Extending the worktree guarantee to reviewers (#359)

#338 fixed the worker half of "the main clone is the human's environment" and deliberately left
reviewers on it — a reviewer is *told* to be read-only with respect to the repo's content (its
template says never to edit files or push, and `gh pr diff`/`gh pr view` are enough for most
reviews without ever needing to). At the time of #359 that was **instruction-backed, not
structural**: a reviewer's CLI was never launched with any deny flags at all, so nothing at the
CLI level denied it `Edit`/`Write`/`git commit`/`git push`; it worked because the reviewer role
was asked to stay read-only, not because it was unable to do otherwise. **#462 made half of it
structural** — the file-editing tools are now denied at the CLI (`Containment::NoEdits`; see
"Reviewer containment: what is structural and what is not" below for exactly which half, and
why the other half stays instruction-tier). What this section is about is narrower and *is*
still true regardless of either: a reviewer is not
read-only with respect to the clone's *checkout state*: `gh pr
diff`/`gh pr view` need no checkout, but a reviewer that wants to run tests locally has always
been told "checking out the PR branch locally is fine" — in the shared main clone, the same one
every other reviewer and the orchestrator's own `git fetch`/rebase traffic uses. That was a live
incident, not a hypothetical: in one session, rev-36 (delta-reviewing a PR) checked branches out
in the main clone and restored it to the default branch when done, while rev-38 (aggregate-
reviewing a different PR) was mid-review in the *same* clone — rev-38 got switched off its
branch mid-review and had to re-verify against `origin` refs from scratch to finish (issue #359).

Three shapes were on the table (the issue named them): extend the worktree default to reviewers
(symmetric with #338); checkout-free review guidance (`gh pr diff`/`git diff origin/A...origin/B`
only — rev-38's own recovery path, and it works, but a reviewer that wants to run tests locally
still needs a checkout *somewhere*); or a hybrid (checkout-free by default, worktree only when a
review brief asks for local test runs). **The human picked the first, explicitly, in the issue's
own comment: "Extend worktree requirement... orchestrator/workers should not touch the main
checkout as this is the humans."** Simple and symmetric — a reviewer is now covered by the exact
same mechanism as a worker (`needs_dedicated_workspace` in `mcp.rs` now matches
`Role::Worker | Role::Reviewer`; every one of the three guards above — worktree default/reject,
fresh-spawn cwd reject, resume cwd inherit-or-reject — applies to a reviewer identically), rather
than adding a second, checkout-free code path that would need its own set of guards and its own
drift risk against the worker one. It also matches the session's own evidence: reviewers in
practice run tests locally far more often than not (every reviewer in the incident's session
did), so a checkout-free default would have merely relocated the conflict to whenever a reviewer
*did* need one.

**A reviewer's worktree cannot simply check out the PR's own branch, though — that is the one
piece #338's worker mechanism doesn't hand over for free.** `git_worktree_add` cuts a *new*
branch off the default branch (`agent/<id>`-shaped, same as a worker's) — sensible scratch space,
but the PR under review is a *different* branch, almost always already checked out somewhere
else (the worker's own worktree, if it's still around). Git refuses to check out the same branch
in two worktrees at once, so a reviewer that ran a bare `gh pr checkout <n>` — which checks out
the PR branch *by name*, creating or moving a local branch to track it — would collide with
whatever else already has it, reproducing a narrower version of the exact incident this fix
exists to close. The fix is a **detached-HEAD checkout**, not a new mechanism: `gh pr checkout
<n> --detach` fetches the PR's head commit and checks it out with no branch name attached, so it
can never collide with anything, in any worktree, ever — multiple worktrees can even sit at the
*same* detached commit simultaneously. This is documented, not code-enforced (there is no MCP
tool wrapping `git`/`gh` checkout subcommands, so nothing can force which flavor a reviewer
runs) — `reviewer.md`'s **Review protocol** step 1 states it as the convention, and the
worktree's own kickoff note (`spawn_agent_ex` in `mod.rs`, role-aware for a reviewer) repeats it
at spawn time so it survives even a fast first read. A reviewer's read-only-with-respect-to-push
convention is unaffected either way: `git commit`/`git push` are denied at the tool level only
for a planner (`Containment::ReadOnly`) — #462 gave a reviewer the editing-tool half of that
tier, deliberately not the git half, since a reviewer's shell IS its job — so no-push is, and
remains, taught in `reviewer.md`, and a dedicated worktree changes nothing about that; it only
gives the reviewer a workspace of its own to sit in while it does what it already does.

### A capability class is never acquired by omission (#544)

`spawn_agent`'s `kind` used to default to `worker`. That reads like a convenience — most spawns
really are workers — but it makes the failure mode of *forgetting a parameter* "silently receive
more capability than intended", because `worker` is the **most**-privileged class loomux can hand
out: a read-write pane with the editing tools and `git commit`/`push` a reviewer and a planner are
both denied. It fired: an orchestrator spawned three reviewers with `kind` omitted and got three
workers, each carrying a brief that said "review PR #536" and "record your verdict with
`review_verdict`", one of them literally named `rev: #536 question-guard`. Nothing objected — not
the pinned per-role model/CLI guardrails (they applied correctly, to the *worker* role), not the
reviewer-only tool gate (`review_verdict` is denied to a non-reviewer, so the pane would simply
have been unable to do the job it was sent to do), not the name. The orchestrator caught it by
reading the return value and killed the panes before they acted.

The distinction that makes this its own defect rather than an operator slip: **#448, #462 and
#465 all harden a pane that was correctly classified** — planner/reviewer read-only enforcement,
the reviewer CLI deny list, the deny-list's own fail-open direction. None of that layer is
reachable when the classification itself is wrong, and a fail-open default puts the classification
one forgotten argument away from wrong. It is the omission twin of #222, which stopped an
*unrecognized* `kind` (`"revieweer"`) from falling through `_ => Role::Worker` to the same place.

The fix is the issue's option 1: a **fresh spawn must name its capability class** — `kind`, or a
`block` (whose kind is authoritative anyway, and which is how a custom-workflow group spawns
everything). Naming neither is refused, loudly, with a message that names both ways out and the
`resume_session` alternative. Option 2 (keep the default, refuse *reviewer-shaped* briefs at
worker class by sniffing the task text for `review_verdict` or the name for `rev:`) was not taken:
it is a heuristic over an open set of phrasings, so it would refuse some correct spawns and miss
the next mis-spawn that happens not to say "review", and it leaves the fail-open default in place
for every class pairing it *doesn't* model (a planner-intended spawn, say). Requiring the class
removes the error class instead of narrowing it, and the cost is one argument per call site.

Three things about the shape:

- **`resume_session` is deliberately exempt**, and that is not a hole. A bare resume inherits the
  resumed session's own block (#254) — it re-derives nothing from `kind`, and an unknown session
  is already a hard error rather than a silent worker. Inheritance is a *stricter* answer than a
  default, so making a resume name a class too would only invite an orchestrator to guess one and
  re-role a conversation by accident. The requirement is on the *fresh* path, where there is no
  prior class to inherit.
- **`kind` stays optional in the JSON schema, deliberately — not because the shape is
  inexpressible.** JSON Schema can say "kind or block": an `anyOf` over three `required`
  alternatives (`kind`, `block`, `resume_session`) would let a strict client catch the omission
  before the round-trip. It was not taken because a strict client mis-rejecting a *valid* call is a
  worse failure than the round-trip it saves, and because this repo's schema is advertisement
  rather than enforcement anyway — the `enum` on `kind` has never been checked against incoming
  arguments; `mcp.rs` re-validates every argument itself. So the tool description states the
  contract and `call_tool` enforces it, three lines from the #222 unknown-kind rejection. If a
  future client tier ever wants machine-checkable arity, `anyOf` is the shape to add, and this
  paragraph is the record that it was considered.
- **Two smaller fail-open fallbacks went with it.** `kind` is now `Option<Role>` all the way
  through the spawn arm rather than a `Role` pre-seeded with `Worker`, which surfaced a second
  place the old default leaked: a bare resume of a pre-#222 roster row whose recorded `role`
  no longer parses used to fall back to `kind` — i.e. to `Worker` — and now refuses instead. The
  one remaining `unwrap_or` (the `role` argument handed to `spawn_agent_ex`, unreachable with
  `kind: None` unless a block is set, which overrides it) resolves to `Planner`, the least
  privileged class, so that even a future path reaching it cannot acquire write capability by
  omission.

### Pane naming & rename precedence (#95r)

A pane's name should say what the agent is *doing*; failing that, it must at least agree
with the pane's `W <seq>` badge (issue #75), never disagree with it. Two rules:

- **Default name = the minted id.** A spawn with no meaningful name (initial workers, or
  any `spawn_agent` with a blank `name`) derives its title from the id `spawn_agent_ex`
  mints — `w-2` → `worker 2` — so title, roster row, and badge all read the same seq. (The
  old per-launch `worker N` counter drifted from the global seq, producing the reported
  "worker 1" pane wearing a "W 2" badge.)
- **`rename_agent(agent_id, name)`** (orchestrator-only, group-scoped, alive-only, audited)
  lets the orchestrator retitle a pane to its task. Names carry a **source tier** —
  `human` > `orchestrator` > `default` (`NameSource`) — and a rename applies only from an
  equal-or-higher tier. So the orchestrator can relabel an id-default (or its own earlier
  name), but a human's in-pane rename (F2/double-click, synced to the backend via the
  `orch_agent_renamed` command at the `human` tier) is never clobbered by a later
  `rename_agent`. Every accepted rename updates the roster and emits `orch-rename` so the
  open pane's title follows; the backend only emits renames it accepted, so the frontend
  needs no precedence guard of its own.

## Launcher UX

"New agent pane" dialog gains a **Mode** select:

- **Single pane** — unchanged.
- **Multiple panes (N)** — spawns N identical agent panes; a worktree name becomes
  `name-1 … name-N` so each agent gets an isolated worktree. (Secondary request.)
- **Orchestrator + workers** — requires a repository; fields: initial workers (0–6),
  max live agents (1–12), a **per-role CLI + model** row for each of orchestrator /
  worker / reviewer / planner (the top *Agent* select is the group default that seeds
  every role; each role can override it — issue #4), and permissions. Spawns one
  orchestrator pane (badged `ORCH`) plus N idle workers (badged `W`), all sharing a
  group color shown as a header dot + pane accent. Reviewers get `REV`, planners `PLAN`.
  Changing a role's CLI re-populates its model suggestions; every distinct role CLI is
  PATH-checked before launch so a missing CLI fails fast and legibly.

## Persistence & resume

Group id is derived from the repo (slug + hash), so relaunching an orchestrator on the
same repo reuses the same state dir: `state.json` (opaque orchestrator-managed queue/
plan/notes) and `audit.jsonl` carry over. The orchestrator template instructs it to
`get_state` at session start and `set_state` + update GitHub issues after every planning
change, keeping issues (label `agent-managed`) the durable source of truth.

## Audit log

`audit.jsonl`, one JSON object per line: every tool call (actor, tool, args, result),
prompt delivery (full text), spawn/bind/exit, state writes. Append-only, human-readable.
Rolls over to `audit.1.jsonl` past 8 MB (one generation kept); full prompt texts land
here, so it grows fast.

**In-app viewer** (`auditview.ts`, `orch_audit` command): every orchestration pane (not
just the orchestrator — the log is per-group and read-only) has an `Alt+A` overlay that
renders the log as a timeline, filterable by actor / action / agent with free-text search
over the detail, and rows expand to show the verbatim prompt/task text. The backend read
(`OrchRegistry::audit_log`) concatenates the rotated generation before the current one so
rotation is invisible to the viewer, parses with a pure, per-line-fault-tolerant
`parse_audit_lines` (a malformed line never sinks the view), and caps to the most recent
`AUDIT_VIEW_LIMIT` (5000) entries to bound the payload against a near-8 MB pair. Live-follow
is frontend polling (`orch_audit` every 1.5 s, sticks to the bottom when the human is
already there) rather than backend event emission: auditing is best-effort and written from
several call sites (including background delivery threads via the free `append_audit`), so a
uniform poll that also absorbs rotation is simpler and more robust than threading an
`AppHandle` through every writer. The overlay reuses the git/task-board floating mechanics
(`.git-overlay`) so it never resizes the PTY — a ConPTY resize repaints and duplicates TUI
frames into scrollback.

## SW-dev process (encoded in templates, not code)

Orchestrator: intake → GitHub issue (`agent-managed` label) → plan → mergeability
assessment (sprawling change ⇒ serialize; independent ⇒ parallel worktrees) → delegate →
monitor → reviewer per PR → **findings dispositioned** → high-level completion check → hand
to user for merge. Workers: branch → implement → meaningful unit/functional tests (test
intent, not vacuous passes) → **red-before-green evidence** → design notes + user docs →
commit → push → `gh pr create` → report. Workers keep quick local iteration
capped at `-j 4` and defer full/longer-running validation to CI — see the
`ci-validate` skill (#320) — CI stays the sole authority for the CI gate.
Reviewers: `gh pr review` with findings, each labelled blocking/non-blocking →
report.

"Dispositioned", not "addressed": an approval that leaves findings behind is not done. The
default is to fix a non-blocking finding in the same PR before merging (bounded like the CI
gate — three rounds and the PR settles); deferring costs a reason saying why the fix doesn't
belong in *this* PR, a filed follow-up (which parks the finding in the label funnel, so it is
not a discharge) and a word to the human. A finding that contradicts the change's own stated
rationale is blocking whatever the reviewer labelled it — and a blocking finding is a `fail`
verdict, never a `pass` that mentions it, or the gate opens on prose it cannot read. A
*question* the orchestrator asked the human (a decision it awaits — not a status line it
announced) holds the merge even where auto-merge, a one-time grant or supervised dangerous
mode would otherwise allow it. The policy and the live incident that produced it are in
`doc/design/workflows.md` → **Findings disposition**.

**The bind is on the verdict, not on the `gh` flag** (#239, carried forward from #238's
review of the same arc on `main`). The recorded verdict above is the *gate's* record, and it
only exists for a group whose workflow declares one; the reviewer's *GitHub-facing* record is
the review it posts, and there the original rule — "a blocking finding means
`--request-changes`, not `--approve`" — is **unsatisfiable**: GitHub refuses both flags on a
PR opened by your own account, which is the normal case when a whole group authenticates as
one GitHub user (every review this repository has received is `COMMENTED`). A rule anchored on
a flag nobody can use binds nothing, while the channel the orchestrator actually merges on —
the verdict the reviewer *states* — stays unconstrained: label a finding blocking, report
`approved`, and every sentence is satisfied. That is the #222 incident rebuilt by the rule
written to prevent it. So the binding record is **the verdict stated at the top of the review
body and repeated in `report(...)`**; `--comment` is the named fallback when the flag is
refused; and a `--request-changes` GitHub refused is never a reason to `--approve`, to soften
the verdict, or to record a `pass`. The two surfaces are complementary — the recorded verdict
is what the *gate* reads, the stated verdict is what the *orchestrator* reads — and an ungated
group has only the second, which is why it cannot be left to the flag. `reviewer.md` and
`mechanics_core(Reviewer)` carry it in lockstep, for the reason every reviewer duty does: a
`mode: replace` persona never reads `reviewer.md`.

### The verdict notice is a signal; the record is elsewhere (#850)

Text typed into a pane is not a message an agent reads once. It joins that agent's
conversation and is re-sent with every subsequent request, so a paragraph delivered to the
orchestrator is paid for again on every turn it takes afterwards — which makes pane text the
most expensive prose in the system, and the orchestrator's pane the most expensive pane. A
reviewer's verdict used to arrive there **twice in full**: once as the `[loomux] … recorded
verdict …` courtesy notice, carrying the whole summary (up to `MAX_SUMMARY_CHARS`, 4000), and
once more as the reviewer's own `report(...)` restating it. Measured over one review round of
eight verdict events: ≈15k duplicated tokens, resident.

Both halves are fixed where they can actually be enforced. The notice's copy of the summary is
capped **in the tool** (`report::verdict_notice_summary`, 400 characters plus a fixed pointer
at `list_verdicts` and the PR) — a cap the code applies, not a length a template asks for,
which is the same argument `truncate_note` already makes for a structured report's `note`. The
truncation is *stated*, with the original length, so a reader can tell that there is more and
where to get it. Nothing else is touched: the verdict file and `list_verdicts` keep every
character, and the gate reads the file — so the cap can never change what merges. (The
`review-verdict` audit line is the one place that was already lossy, and stays exactly as it
was: it has always recorded the summary's first 500 characters, which is a record of the event,
not the record of the review.)

The second copy cannot be capped, because it is a different call with legitimate uses, so it is
addressed as prose in the two places a reviewer is taught to record a verdict at all (the gated
block note and `mechanics_core(Reviewer)`): the recorded summary targets ~100 words with the
analysis in the PR review body, and the `report(...)` after it is one line — verdict, ref,
pointer, findings count. `reviewer.md` carries the report half only, since an ungated group has
no verdict tool to describe (the same asymmetry every verdict rule in this section has).

### Engineering standards, not just process (#236)

The prompt suite's *process* half was strong (gates, bounded loops, externalized state,
disposition) and its *engineering* half was one line long: "does the PR satisfy the acceptance
criteria?" A codebase can answer yes to that on fifty consecutive PRs and still rot, because
acceptance criteria say what a change must **do** and never what it must **be**. #236 gives the
orchestrator a value system to match its operational one:

- **Grounds to send work back** (`orchestrator.md` → *Engineering standards*, the one
  authoritative site; referenced from plan intake and from the completion check). Cross-module
  coupling, a duplicated mechanism, an unargued new dependency, a public-contract change with no
  design note, a change that contradicts a design note, scope drift. Naming one is *blocking*
  whatever the reviewer labelled it — the same call the orchestrator already owns for a finding
  that contradicts the change's own rationale: the reviewer rates the diff, the orchestrator owns
  the requirement **and the architecture**. The gate is sited at **plan intake** as well as at the
  PR, because a design flaw costs one planner comment before code exists and a revert after it.
  `planner.md` owes the matching content — boundaries, reuse-before-invention, dependencies,
  public-contract changes, alternatives considered — since a plan that never named its boundaries
  cannot be gated on them.
- **Red before green, evidenced.** "Tests that would fail if the feature were broken" was in the
  worker's DoD from the start — as an *assertion nobody ever checked*, which is the most common
  quality failure in autonomous coding and is invisible from the diff. Now the worker runs its new
  tests against the base branch, watches them fail *for the expected reason* (not on a compile
  error), and pastes the command and failure line into the PR; the orchestrator treats a `done`
  without that evidence as **not done**; the reviewer verifies it rather than reading it (a quoted
  failure line is text, and text is not a red test). All four surfaces move together —
  `worker.md`, `mechanics_core(Worker)`, `orchestrator.md`, `reviewer.md` — because any one of
  them dropping it restores the status quo.
- **Post-merge ownership.** Auto-merge, a one-time grant and supervised dangerous mode all let the
  orchestrator *land* code, and the prompt then went quiet — nothing told it to watch the default
  branch. A PR green on its own branch still breaks main (a semantic conflict with whatever landed
  under it; a job that only runs post-merge), and a red default branch blocks every worker in the
  group. So a merge it performed makes it the owner of main's next CI run: on red, **stop merging**,
  **fix forward once**, then **revert** (the default — restoring main costs a revert, debugging it
  in place costs everyone's afternoon), and flag the human.
- **Review lanes.** The default reviewer covered correctness, tests, requirement fit, docs and
  style — and nothing on **trust boundaries**, **dependency hygiene** or **algorithmic cost**, in a
  repo where a bad dependency bricks the binary (`getrandom`/`ProcessPrng`) and a trust boundary
  holds only because the webview is trusted (`group_id`). Added to `reviewer.md` **and**
  `mechanics_core(Reviewer)` in lockstep, for the reason the findings duty is: a `mode: replace`
  persona never reads `reviewer.md`, and a lane nobody was assigned is a lane no verdict reflects —
  the gate cannot tell "reviewed and clean" from "never looked at".
- **The learning loop, and filing without starting.** A pattern (a finding class on three PRs, a CI
  failure mode that has burned two fix rounds, a convention reviewers keep re-flagging) gets
  distilled **once** into something durable — a docs PR or a filed convention issue — because a
  review that re-teaches the same lesson every week is how a codebase stays exactly as good as it
  was. And the orchestrator may **file** an issue for debt it observes, with a suggested label,
  though it may never **start** one: the label funnel governs what it *begins*, not what it
  *notices*, and filing it is not doing it (it parks in the funnel exactly like a deferred
  finding). Autonomy at zero consent cost.
- **Post-merge re-sync of the fleet.** #236 asked only for *detection* — add `--json mergeable` to
  the sweep and route a `CONFLICTING` PR to its owner. That fires at the most expensive possible
  moment. The rule shipped instead is the one a human maintainer actually follows: **the default
  branch moving is an event**, whoever moved it (the orchestrator's merge, the human's, one it
  merely observed), and every open branch behind it is then **stale** — which is *not* the same as
  conflicted. A branch that still merges cleanly was reviewed, tested and CI'd against code that no
  longer exists, so its green checks describe the past. After any merge (and again on the sweep, as
  the backstop for drift nobody saw), every open PR is rebased onto **the branch it will merge
  into** — a sub-PR onto its integration branch, not reflexively onto `main`. A clean rebase the
  orchestrator does itself (mechanical, no delegate slot); the first real conflict routes to the
  **owning** worker's resumed session, **one attempt**, then the human — the CI gate's bound, for
  the CI gate's reason. The rebase is a push, so CI re-runs and every verdict goes stale: that cost
  is the argument for paying it early and in the quiet rather than on the PR you were about to
  land. Paced against the delegate cap, never bursty.

- **Compression, and the INVARIANTS digest.** The prompt predicts its own compaction ("your
  context may have compacted"; "compact at lulls") and was nonetheless written as ~500 lines of
  prose optimized for one careful read, with the load-bearing rules restated three and four times.
  Repetition is not memory: a summary keeps a document's *shape* and loses its *rules*. So the
  eleven rules whose loss is dangerous — the merge gate, the question-hold, disposition, the
  architectural bar, red-before-green, red main, fleet staleness, the label funnel, bounded loops,
  one-task-per-worker, externalized memory — are stated **once**, in an `## INVARIANTS` digest
  leading the bulk of the document (#381 later put a short **Your first turn** call-sequence
  primer ahead of it, but the digest still precedes every heavier policy section), which the
  orchestrator is told to re-read at session start and after every compaction. Every body
  section then *stops restating them* and holds only the procedure and the why, cross-referencing
  the digest by number. That is what pays for the additions above: the
  orchestrator template grew seven new rules and still ends up denser than it was
  (≈513 → 625 lines for ~2× the rules), because the rhetoric that carried the old ones is gone.

- **The rules that bound the rules** (rev-21's review of the above). Four of the new rules were
  executable-by-a-literal-agent failures, and they are the same species as #235's:
  - **Red-before-green needs an exemption, or it refuses the work it exists to enable.** Stated
    unconditionally, it bounces every PR that legitimately adds no test — including the two this
    very design prescribes: the learning loop's **docs PR** and a red main's **revert**. So the
    exempt class is enumerated once, in `worker.md` (four members: docs/comment-only, a revert, a
    pure rename/move the suite already pins, a re-blessed golden), and it **costs one line**: the
    PR names which class it is and why, with the suite green. "There was nothing to test" is a
    claim like any other — stated, it is reviewable; unstated, it is indistinguishable from an
    untested feature, which is what the rule was written to stop.
  - **"Stop merging until main is green" forbade the merge that makes main green.** The freeze is
    on *feature* merges; the fix-forward or revert PR is the exception, because it is the exit
    from the state. Without that clause a literal orchestrator halts and waits for a human — in
    auto-merge, the unattended mode the rule was written for.
  - **The learning loop may not dispatch its own artefact.** "A docs PR — dispatch it as a normal
    work item" was an opt-out from the label funnel sitting three sections below the label funnel,
    and it inverted the policy: a finding a *reviewer* raised has to park in the funnel, while a
    pattern the orchestrator noticed *by itself* could be started directly. It files the lesson
    with a suggested label and stops, like everything else.
  - **The architectural bounce is bounded** like every other loop: one bounce, naming every ground
    it has; a second disagreement is a question for the human, not a second bounce.
  - And the re-sync has a **topology license**: rebase the *merge frontier* (the PRs targeting the
    branch that actually moved), let a deeper stack wait for its own base, batch on deep stacks.
    Because a rebase re-stales every verdict, re-syncing an n-deep stack after every sub-PR merge
    costs O(n²) *re-reviews*, not just rebases — and a PR held on an unanswered question is left
    alone entirely: it isn't going anywhere, and re-staling it buys a review nobody can act on.

Each rule is pinned in `tests/workflow.rs` on the surfaces that carry it, and the golden fixtures
in `tests/fixtures/pre222/` are re-blessed in their own commit — the diff on that directory is the
review surface for "what did we just tell every default group to do differently?". The pins match
**substance with whitespace collapsed** (`flat()`), deliberately: a pin that fires when a
paragraph is re-wrapped is a pin that teaches people to re-bless without reading.

`tests/prompts.rs` (from #238, which front-ran this arc's policy half onto `main`) pins the same
rules on the **default rendering** — the templates as an ungated group reads them, with no
workflow file and no placeholders substituted in. The two suites are complementary and both are
kept green: `workflow.rs` pins the duties across *both* surfaces a reviewer can reach
(`reviewer.md` and `mechanics_core`) and pins the machinery vocabulary; `prompts.rs` pins the
region-scoped prose of what every group gets by default. A rule that lives in only one of them is
a rule one kind of group is not being told.

**A pin is a claim until it has been watched failing** — the suite's own rule, applied to itself
(rev-21 F1). The first cut of the compression pin asserted that the body did not restate what the
digest owned, anchored on a phrase *the compression had deleted*: it read `0 <= 1` and could not
fail in either direction. Worse, nothing pinned the orchestrator's #235 policy at all — deleting
it turned exactly one test red, the **byte fixture**, whose message says "re-bless me", which is
the red this very design calls the one that teaches people to re-bless without reading. So:
`the_orchestrators_findings_policy_survives_in_substance_not_just_in_bytes` asserts each rule of
the policy **inside the section that owes it** (a document-wide match lets the digest's one-line
copy rescue a body section someone gutted), one assert per rule, so a deletion *names what it
deleted*. Single-word anchors (`"full"`, `"stale"`) are banned: whitespace-collapsed matching on a
generic word is close to a tautology.

**Prose pins have three failure modes, and only the first is obvious.** Each was found by mutating
the templates, never by reading the tests — the progression is the reusable part:

1. **The anchor no longer exists.** `body.matches("an approval with findings") <= 1` against a
   document that no longer contains the phrase: `0 <= 1`, green forever, in both directions.
2. **The anchor exists twice and the rule lives in only one of them.** Every load-bearing rule now
   appears in the digest *and* in the body by design — the rule, and its procedure. A
   document-wide `contains` is satisfied by either, so deleting the body's procedure leaves the
   pin green, rescued by the digest: the rule survives as a slogan with no instructions attached.
   Fixed by scoping every assert to the region that owes it (`section()`), which is why the pins
   read `disposition.contains(…)` and `aftermath.contains(…)` rather than `orch.contains(…)`.
3. **The anchor's words appear in unrelated prose inside that same region.** `"groom"` was rescued
   by the `agent-ready` bullet three paragraphs above the prohibition ("the issue is *groomed* and
   ready to build"); `"one line"` in `worker.md` by the report guidance ("one line restating the
   task") — which meant the red-before-green exemption's **price**, the stated reviewable claim
   that is the entire safety of the exemption, could be deleted in silence; `"question for the
   human"` by the Engineering-standards section's own ambiguous-case sentence. The fix is to
   anchor the rule's **own clause**: `"groom an issue the human hasn't"`, `"naming which of"`,
   `"no longer a bounce"`.

Two rules follow, and together they make the dead pin a *test failure* rather than a discovery:

- **The mutation harness deletes the rule, not the string.** Deleting every occurrence of a phrase
  measures whether the pin can see the *phrase* vanish, which is not the question anyone is
  asking. The harness deletes the markdown unit — the list item or paragraph — that carries the
  rule, inside the region the pin scopes to, and requires the owning test to go red: **60/60
  rules**, one at a time. That is what surfaced failure mode 3, and (on its own first case list) a
  fourth instance of mode 2: `"fix forward once"`, rescued by INVARIANT 6's one-line copy while
  the red-main *procedure* was gone.
- **An anchor must occur exactly once in the region it is asserted in** — enforced by `pinned()`,
  which every #236 anchor goes through. This is failure mode 3 made *mechanically impossible to
  reintroduce*: an anchor that appears twice in its own region cannot fail when the rule it names
  is deleted, because the other occurrence rescues it, so it is a **red test right there** instead
  of a defect someone finds later by mutating prose. It pays immediately — it rejected `O(n²)` in
  the re-sync section (the depth clause and the fan clause both name the cost, so either would
  have rescued the other) and forced the depth rule onto its own clause.

A pin you cannot make fail is worse than no pin: it is a claim of coverage. The uniqueness rule is
what keeps that claim honest without anyone having to remember to re-run the harness.

## Validation-round additions (2026-07-03)

- **Init friction / permissions**: agents launch with `--add-dir <group dir>` and
  pre-approved loomux MCP tools so initialization needs no human approvals; the "Auto"
  preset additionally pre-approves `git`/`gh`. Bypass-permissions mode was removed
  entirely — its confirm dialog defaults to "exit", which the typed kickoff would
  accept, killing the pane.
- **Agent CLIs**: groups run either Claude Code or Copilot CLI via per-CLI command
  adapters (`build_agent_command`); the launcher's model suggestions follow the CLI.
  Unknown CLIs fall back to Claude explicitly at group creation, never silently.
- **Concurrent groups per repo**: group ids take the first non-live suffix
  (`base`, `base-2`, …), so parallel orchestrations on one repo never share an
  orchestrator/state, while a relaunch with no live group still resumes `base`'s
  state. Badges carry a group ordinal (`ORCH 2` ↔ `W 2`) plus the accent color.
- **Task board**: structured `tasks.json` per group (statuses queued → in-progress →
  review → pr → human-testing → done, plus blocked and `prototype`; notes; priority
  order), edited by the orchestrator via MCP tools and by the human via the pane overlay
  (Alt+T); each side's edits notify the other, and everything is audited. `TASK_STATUSES`
  is the single source of truth — validated on every write; the frontend picker and MCP
  `upsert_task` enum mirror it.
- **Prototype → Proceed (#147)**: `prototype` is a demo-gate status — a draft the human
  is evaluating before committing it to a release (the `agent-prototype` label's on-board
  home). It renders as a distinct magenta chip and joins the human-gated states the board
  highlights (`isAwaitingHuman` / `attention_tick`'s gate map). Its board action is **not**
  the merge-gate approve/changes but a dedicated **Proceed** (`orch_proceed_task` →
  `proceed_task`, two-click confirm): guarded to `prototype` items (`ensure_prototype`,
  constraint 6), it flips the task to `in-progress` — the item re-enters active work rather
  than parking on the verdict — records a human-attributed note, and delivers exactly one
  typed "promote to a full production build" notice to the orchestrator. Like `approve_task`
  (and unlike `start_task`), the durable status flip carries the decision, so it does **not**
  reject on a paused group — the orchestrator sees the flip + note on resume via `list_tasks`.
  The template documents the loop (build demo → park in `prototype` → on Proceed, run the
  full production round, no corners).
- **Merge-gate actions**: on `pr`/`human-testing` items — the exact point where the
  human gatekeeps — the board overlay exposes the three touchpoints that otherwise
  meant typing into the orchestrator by hand. Issue/PR chips are clickable and open in
  the browser (`orch_open_ref` resolves `#N`/`N`/URL against the repo's `origin` remote:
  `normalize_remote_web_base` + `resolve_ref_url`, both pure/tested; the URL is opened
  via the OS handler as a single argument, never a shell line). **Approve**
  (`orch_approve_task`) marks the item done and types an approval notice into the
  orchestrator to merge; **Request changes** (`orch_request_changes`) collects findings
  in a modal, records them as a board note, and types them to the orchestrator to route
  back to a worker (status stays at the gate). Both go through `upsert_task` (audited,
  actor `human`) and deliver a purpose-built typed notice, staying inside the overlay
  pattern — no PTY resize.
- **Bulk approve (#507)**: the board's existing multi-select (shared with "delete
  selected") also drives **Approve selected (N)**, so a human clearing a batch of
  finished PRs authorizes them in one pass instead of clicking through N modals and
  queueing N prompts at the orchestrator. `approvableSelection` (pure, in `taskboard.ts`)
  narrows the ticked set to rows `canApprove` admits, so ticking a `queued` row for a
  later delete can never inflate the count of grants about to be issued; the dialog lists
  exactly those rows, each with its own optional note. See **Bulk approve: delivery, not
  authority** under the merge gate for what the backend does with them.
- **Per-task sessions**: one task per worker (template-enforced). Claude session ids are
  pre-assigned via `--session-id`; Copilot mints its own and is tracked post-spawn (see
  "Copilot session tracking" below). Either way the id is recorded on roster + tasks, so
  follow-ups `spawn_agent(resume_session, cwd)` into the original conversation/workspace.

- **Kickoff readiness + restore (second validation round)**: kickoffs wait for the
  CLI to paint and go quiet instead of a fixed delay (a loaded machine lost a
  reviewer's kickoff to the startup stdin flush); delivery outcomes are audited.
  A durable per-group roster (`agents.json`) maps session ids to roles, marking
  sessions in the browser and enabling full orchestration restore: a dead group's
  orchestrator session relaunches group + MCP identity + task board via
  `resume_orch_session`, resuming the conversation; workers/reviewers rejoin live
  groups the same way.

## Cost containment (#7)

Orchestration multiplies *unattended* spend: `max_agents` caps width, not duration, so a
group can quietly burn money for hours. Four guardrails, all in the platform (judgment stays
in the prompt), contain that. The two configurable ones live in `Guardrails`
(`idle_kill_minutes`, `max_spawns_per_hour`), are collected by the launcher (0 = off),
persisted in `group.json`, and clamped in `clamped()`.

- **Per-group pause / resume.** A human-only action (`orch_pause_group` / `orch_resume_group`
  Tauri commands; frontend `pauseGroup`/`resumeGroup`/`groupPaused`). While paused, loomux
  types nothing into any of the group's panes — every kickoff, orchestrator prompt and worker
  report is withheld, so agents finish their current turn and idle out rather than being
  killed. The flag is mirrored to a `paused` marker file so a pause survives an app restart
  (re-seeded in `create_group`).

  **A pause queues; it does not discard (#569 option 2, the human's decision).** Until this
  landed, `deliver_prompt` short-circuited *before* the front door: the payload was destroyed,
  the caller was told `Ok`, and a `prompt-suppressed-paused` line was the only record it had
  ever existed. That made pause the last non-crash path where something a sender had been told
  succeeded ceased to exist — a worker's `report("done")` fired mid-pause evaporated, and the
  orchestrator went on waiting for a report that would never arrive, contradicting the "queued
  means safe" model the #445/#523 batch established everywhere else. Option 1 (#586, merged as
  809df7a) made that loss *visible*; option 2 removes it.

  The shape, three layers, none of them optional:

  1. **The front door admits.** `deliver_prompt`'s pause branch resolves `pty_id`, audits the
     ordinary `prompt` line, and calls `enqueue_text` with `EnqueueReason::GroupPaused` — an
     ordinary queue entry, so it inherits every guarantee the queue already gives: FIFO order,
     the per-pane `QUEUE_MAX_PER_PANE` cap, admission-time coalescing of byte-identical
     repeats, the audit trail (`delivery-queued` with `reason: group-paused`), and #467/#468
     durability, which is what makes a pause-held payload survive a loomux restart *during the
     pause*. `pty_id` is now resolved BEFORE the pause check: a queue is keyed by pane, so a
     target with no terminal has nowhere to hold anything, and the honest answer is the same
     `Err` an unpaused delivery to that agent already gets rather than a false `Ok`.
  2. **The front door does NOT start a drainer, and the drainer refuses to paste.** #470's
     invariant is that the admission which observes an empty queue owns spawning
     `run_queue_drainer`; the pause branch deliberately leaves that unmet, because a paused
     group must have nothing running that could type into a pane. `run_queue_drainer` also
     checks `is_paused` each pass — after its `commit_exit`, before it plans a flush — and
     holds without spending a notice, running a hold clock, or raising an escalation badge (a
     pause is not a stuck pane, and badging one would tell the human about a state they set
     themselves). That second layer is load-bearing rather than defensive: `readmit_recovered`
     kicks a drainer of its own when a pane rebinds after a restart, which for a group paused
     across that restart would otherwise paste straight into the pause.
  3. **Resume discharges it.** `resume_group` calls `flush_paused_queues`, which finds every
     pane holding entries stamped with this group and calls `ensure_drainer` on each (audited
     as `pause-flush`, carrying the pane list and whether a drainer could actually be spawned).
     This is the *single* place that discharges the invariant layer 1 leaves open: if it stops
     firing, every payload a pause held is stranded — queued, audited, persisted, and never
     delivered. It is deliberately **not** filtered on liveness: a pane whose agent died
     mid-pause still gets a drainer, whose own first-pass checks then drop the queue through
     the ordinary `commit_exit(force: true)` → `announce_dropped(AgentDied)` path. Skipping
     dead panes would leave those entries in `queues` and in `queue.json` with nothing left to
     look at them — a silent *retention*, the same defect class as the silent discard.

  **A kickoff held through a pause stays a kickoff (#620).** Nothing guards `spawn_agent`
  against a paused group, so a spawn during a pause routes `Delivery::FreshKickoff` through the
  pause branch like any other delivery — and #569 shipped with `flush_paused_queues` calling
  `ensure_drainer(..., None)`, so at resume the brief pasted with `wait_ready: false`,
  `confirm_autopilot: false`, `fresh_kickoff: false`. For a copilot pane under `--autopilot` that
  is a wedge rather than a timing nit: the consent dialog appears after the kickoff Enter (see
  `confirm_copilot_autopilot_dialog`), nobody dismisses it, and #517's `fresh_kickoff` recovery is
  unarmed. It was never a regression — pre-#569 the brief was destroyed outright, which is
  strictly worse — but it is a new *shape* of that loss, surfaced by making the payload survive.

  The human's resolution was option 1 of the three the issue listed (carry the kind / reject
  spawn while paused / accept it), because it is the only one that also covers a brief already
  admitted when the pause lands. `QueuedDelivery` gains a `delivery_kind: Delivery`, stamped at
  admission by `deliver_prompt`'s front door (`enqueue_text_as`) on **both** branches, and
  `flush_paused_queues` reads it back off each pane's front entry — `paused_flush_kickoff` →
  `paused_flush_treatment` → the same `KickoffTreatment` (renamed from `RedeliveryTreatment`,
  which now has two producers) that #517's re-delivery path already fed to `FreshFirstAttempt`.
  Unlike a re-delivery, a pause flush copies the **original** kind's own flags: the pause changed
  when the brief lands, not what it is. The one flag the kind cannot decide alone,
  `confirm_autopilot`, is re-derived through `should_confirm_copilot_autopilot` exactly as the
  front door derives it. The id of the entry the treatment belongs to travels with it, so
  `FreshFirstAttempt`'s guard *drops* the treatment rather than misapplying it if `plan_flush`
  picks a different front entry by the time that pass runs. `pause-flush`'s audit line gains
  `kickoff_panes` — the boot wait and the consent confirm happen inside a thread nothing can
  observe, so the log is the only record they were armed.

  **What the new field means after a restart: nothing, deliberately.** `QueuedDelivery` *is* the
  on-disk record, so the field persists (`#[serde(default)]`, so a `queue.json` from an older
  build still parses and comes back as `MidSession`). But `readmit_recovered` re-admits a
  recovered payload through plain `enqueue_text` — as `MidSession` — rather than reinstating the
  kind on disk: the boot that kind describes belongs to a pane that no longer exists (`pty_id` is
  re-minted at restore and `agent_id` names nothing live), the pane it rebinds to gets its own
  `ResumeKickoff` for the boot it really did perform, and re-arming `confirm_autopilot` across a
  restart would fire a stray Enter at a dialog nobody is waiting on — `redelivery_treatment`'s
  argument for not copying that flag. So a restart mid-pause and an older build's snapshot land
  on the same behavior, and it is the one that takes no action against a live pane.

  **One shape #620 does not close, named by mechanism.** *Any pause that lands between a
  drainer's spawn and its first pass.* `immediate_first_pass` is `iteration == 1` and a pass that
  meets the paused gate spends that iteration on a `continue`, so from iteration 2 the treatment
  is gone — whichever call spawned that drainer. The drainer then holds the pane for the rest of
  the pause (it polls, refusing to paste, rather than exiting), so the resume's `ensure_drainer`
  correctly no-ops for it and the treatment `flush_paused_queues` computed is never applied.
  Stating it by origin ("a pause landing just after an unpaused kickoff's drainer") would imply a
  re-pause landing on a drainer the *resume flush itself* just spawned is immune, and it is not:
  same mechanism, same loss, same fix. Closing it means keeping an unattempted first pass alive
  across a pause hold, which changes what `immediate_first_pass` means for the hold-escalation and
  still-queued-notice gates as well. #620 stays open for that.

  **The flush header names the right hold.** `queue::FlushCause` (`PaneBlocked` /
  `GroupPaused`, chosen by `flush_cause` over the batch's admission reasons, never over the
  live `paused` flag — by flush time the group is unpaused by definition) picks between "queued
  while this pane was blocked" and "queued while this group was paused". The pre-#569 wording
  was the only case there was; sent for a pause-held payload it would point its reader at a box
  or a dialog that never existed. A mixed batch reports the pause: the pause is the more recent
  hold and the one that decided when the flush happened. The drainer also forces
  `header_pending` on for a `GroupPaused` cause — a lone delivery held through a pause meets a
  *fresh* drainer at resume whose first pass is the uncontended shape, and would otherwise land
  as a bare payload with nothing saying it had been waiting since before the pause.

  **What the contract change costs, stated.** Drain-on-resume re-spends tokens the human paused
  to stop — that is the trade the human decided, on the argument that a pause which loses a
  worker's `report("done")` costs more than the tokens it saved, because the orchestrator then
  waits forever on a report that is never coming. The containment *while paused* is unchanged:
  nothing pastes, so no agent is woken. Two human-facing entry points still reject up front
  rather than queueing — `steer_orchestrator` and `start_task` — because both are a human acting
  *now*, and deferring their action to a resume they have not chosen yet is not what the button
  promises; the rejection's *reason* changed with #569, its answer did not.

  **A pause can still destroy a delivery, and saying otherwise was the review's B2 finding.**
  The per-pane cap did not go away: a pane already at `QUEUE_MAX_PER_PANE` (8) has further
  admissions refused by `enqueue_text`'s `RejectFull` arm, which drops the payload and returns
  `Err`. The first draft of this note, and three doc comments with it, framed that as a
  theoretical residual and claimed the only way to meet a destroyed payload was to pause under
  an older build. Both halves were wrong, and the second one made the first worse:

  - The cap is per PANE and the orchestrator's pane is where a whole fleet converges.
  - **Most of loomux's own advisories now queue there too.** Only about ten of the 31
    `deliver_to_orchestrator` call sites carry a pause guard; the toggle notices
    (`auto_merge_notice`, `auto_release_notice`, `dangerous_mode_notice`,
    `workflow_mode_notice`), `max_agents_notice` and the board/task notices do not, so before
    #569 they were destroyed by the pause branch and now they occupy queue slots. That is a
    *consequence* of option 2 rather than a decision anyone took, and it is what makes the cap
    reachable: a human who pauses, flips two toggles, adjusts the cap and leaves it for an hour
    can have eight advisories queued when a worker's `report("done")` arrives ninth. The worker
    gets `Err`, retries fail for as long as the pause lasts, and it idles out.

  So the honest claim is narrower: a pause no longer destroys deliveries **at the pause
  branch**, and the cap is where the remaining loss lives. `suppressed_during_pause` therefore
  scans the window for `delivery-dropped` lines carrying `enqueue_reason: group-paused` as well
  as for the legacy `prompt-suppressed-paused` ones, and the resume notice reports both —
  separately, because what a reader should DO about them differs (see below). The
  `enqueue_reason` filter is what keeps this scoped: `delivery-dropped` is also written for
  ordinary queue-full rejections, and those are the sender's own synchronous `Err` to deal with,
  not something a resume should re-report.

  **Whether those advisories SHOULD queue is a human's call, and is deliberately left open.**
  Adding a pause guard to the other ~21 sites would keep the cap out of reach, at the cost of
  losing advisories a resume could otherwise deliver; leaving them is the status quo this PR
  produced. Both are defensible; neither is a worker's decision to take silently, which is why
  it is written down here rather than resolved in the diff.

  **The admit/resume race, and why there is no lock** (review B1). `deliver_prompt` reads the
  pause flag and then admits, with real I/O in between (`recover_persisted_queue`, the durable
  snapshot); `resume_group` clears the flag and only later snapshots `queues`. A resume landing
  in that gap sees an empty queue, starts nothing, and the admission then lands in an unpaused
  group with no drainer and — since the pause branch is the one path that deliberately never
  calls `ensure_drainer` — nobody left to start one: the #470-B1 stranding class, reproduced by
  this fix. The close is to re-read the flag AFTER the admission and nudge `ensure_drainer` if
  it is now clear, which orders the two sides against each other rather than merely letting
  them coexist: resume-then-admit is caught by the re-check, admit-then-resume by the flush.
  Both firing is harmless, because `ensure_drainer` no-ops for a pane already draining — which
  is exactly why a redundant nudge can be spent freely where a lock could not. The nudge is
  audited (`pause-race-nudge`) because it is otherwise invisible: a headless test cannot observe
  `ensure_drainer` at all, so the audit line is what
  `a_resume_racing_an_admission_never_strands_it` asserts, over 24 real threaded interleavings.

  That test is an invariant **guard**, not evidence the re-check is load-bearing, and the
  distinction is recorded because conflating the two is how a coverage claim outlives the thing
  it claimed to cover. Removing the re-check does not redden it — checked, not assumed (run
  30704213481) — because the losing interleaving needs the resume to land between the flag read
  and the admission, and the stagger that makes the pause branch reachable at all also gives the
  entry time to be queued before `flush_paused_queues` looks. Forcing that window open needs a
  seam in the code under test, and a test of a seam is not a test of the race. The re-check is
  justified by the argument above, and by costing nothing when it is unnecessary.

  **The resume-time loss notice, now covering two causes.** Nothing writes
  `prompt-suppressed-paused` any more, so that half needs a group paused under a build that
  discarded and resumed under one that queues — real, and the reason the scan was kept rather
  than deleted. The other half (`SuppressedCause::QueueFullDuringPause`) is not history at all,
  per B2 above. The notice separates them because the reader's next question is "can this happen
  to me again", and the answers differ: the legacy discard cannot recur on this build, a
  capacity refusal will recur on the next long pause. Each cause contributes a sentence only when
  that window actually contains it — explaining a failure mode this pause did not have is one
  more paragraph between the reader and the one that matters — and each listed item names its own
  cause, since a single window can mix them. It also states the current rule outright ("anything
  the pause merely HELD is delivering on its own right now; do not re-request that"), because
  both behaviors now coexist in a group's history and a reader who took the loss wording as
  universal would re-request work already on its way — the duplicate `queued_notice`'s "do NOT
  re-send" exists to prevent, arriving from the other direction. Its bounds are unchanged and still load-bearing for that path:
  `queue::dropped_payload_preview` caps each preview at `DROPPED_PREVIEW_MAX` (160 chars,
  truncation marked), and — the bound that matters, since *count* is what a pause controls —
  the notice names at most `PAUSE_SUPPRESSION_LIST_MAX` (8) individually and summarizes the
  rest as a count pointing at the audit log, which holds every discarded payload in full. The
  window is bounded by the most recent `group-pause` line and nothing else; `group-resume` is
  deliberately not a boundary, because `create_group` audits that same action name for a group
  RESTORED from disk. If the marker has rotated out of the readable log the notice says so
  rather than presenting a possibly over-counted list as exact. And it must not become the next
  silent loss: it is an in-band delivery, so it fails exactly when a pause has run long enough
  for the orchestrator to idle out. When it fails, every distinct live target pane gets the
  `StrandedBlocker::PauseSuppressed` badge instead — the channel #563 established for holds an
  orchestrator-targeted notice cannot reach — and a pane already carrying another mechanism's
  badge is left alone.

  **The loss notice is admitted past the cap, by exactly one entry** (rev-128). It is delivered
  with `queue::EnqueueReason::PauseLossNotice`, the only reason `queue::admit` lets exceed
  `QUEUE_MAX_PER_PANE`. Without that, the flagship case defeated itself: the pane a long pause
  fills first is the ORCHESTRATOR's, because that is where a fleet's reports converge, and it is
  still at capacity when the human resumes — so the notice reporting the destroyed payloads was
  *certain*, not merely at risk, to be destroyed by the same cap. The badge fallback did not
  cover it either: by resume that pane already carries `note_queue_capacity`'s at-capacity badge,
  and `pause_badge_decision`'s never-stomp-another-mechanism's-badge rule then skips the pause
  badge, leaving the audit tally as the only trace. Notice refused, badge suppressed — the #569
  stall one level up, inside the fix for it.

  The exemption is bounded rather than a hole, and each bound is load-bearing: one entry
  (`PAUSE_LOSS_NOTICE_HEADROOM`), one notice per resume, emitted only when something was actually
  lost, and reachable from exactly one call site because `deliver_prompt_as` /
  `deliver_to_orchestrator_as` are private — a `pub` door with a reason parameter would make that
  bound a convention instead of a fact. Coalescing still runs first, so a byte-identical repeat
  folds in rather than spending the headroom. A second resume that finds the headroom still
  occupied is refused like any other delivery and records `delivered: false` rather than claiming
  the orchestrator was told; that refusal path is tested
  (`the_loss_notice_headroom_is_one_entry_and_a_second_resume_is_refused`), which is the honest
  way to cover it — the alternative on offer, an order swap, does not work: `flush_paused_queues`
  only *starts* drainers, so the queue is still full microseconds later when the announce runs.

  **Advisory notices stay suppressed while paused, and that is not the same question.** The
  four `notice-suppressed` paths (`notify_queue`, the unconfirmed-delivery notice,
  `flush_unconfirmed_notices`, `notify_delivery_confirmed_late`) plus the low-disk backstop and
  the watchdog / idle-tick / compact-nudge / workflow-reload skips are loomux's own commentary
  *about* a delivery, not the delivery itself, and each already leaves an audit line. The
  payload they describe is now durably queued and flushes on resume with an accurate header, so
  replaying stale advisories at resume would be noise about work that has already arrived.
- **Idle-worker auto-kill.** Each worker/reviewer carries `idle_since_ms`, stamped when it is
  spawned without a task or reports `done`/`blocked`, and cleared when the orchestrator sends
  it a prompt (`send_prompt`). A background reaper (`start_idle_reaper`, 30s tick) kills any
  whose idle time crosses the group's `idle_kill_minutes` and notifies the orchestrator so it
  can respawn on demand. The threshold logic is the pure `idle_should_kill`; the orchestrator
  is never a candidate. Off by default (0) — the human opts in, since auto-killing is
  destructive-ish.
- **Per-group cost aggregation.** `group_usage` sums each live pane's session cost into one
  summary (total + per-agent). Cost is parsed best-effort from the pane's in-pane statusline
  (`parse_session_cost` scans the ANSI-stripped tail bottom-up for the freshest `$` figure);
  panes without a visible cost contribute `null` and are excluded from the total. Surfaced
  both to the orchestrator (MCP tool, for status summaries) and the UI (`orch_group_usage`).
  The registry-level value is always the full per-agent table; the MCP `group_usage` tool
  (#866) defaults to summarizing it instead — group/live totals, the top 10 agents by total
  tokens, and a `rest` rollup (split live vs historical) for everyone else — with
  `detail: true` as the escape hatch back to the full table. The UI's poll path is
  unaffected; it always reads the registry-level value directly.
- **Spawn-rate limit.** `max_spawns_per_hour` is a runaway-orchestrator backstop: worker/
  reviewer spawns are counted over a rolling hour (`spawn_rate_exceeded`, checked+recorded
  under one lock in `check_and_record_spawn`) and refused past the cap. Only spawns that pass
  the gate are recorded — a refused spawn is not counted, so the cap can't lock a group out;
  a spawn admitted past the gate but later aborted (worktree/bind failure) still counts. The
  orchestrator pane itself (human-launched) is exempt. Off by default (0 = unlimited).

## Copilot session tracking & resume parity (#12)

Claude accepts a pre-assigned `--session-id`, so its per-task session is known and recorded
at spawn. Copilot has `--resume <id>` but **no** way to pin an id up front — it mints one and
writes `~/.copilot/session-state/<id>/workspace.yaml` a few seconds into boot. That gap left
Copilot groups without resumable per-task sessions, session-browser chips, or full restore.
The fix closes it without ever pre-assigning:

- **Baseline + watch.** Just before a Copilot pane's CLI starts, `spawn_agent_ex` snapshots the
  session ids already on disk (`copilot_session_ids`). After the pane binds, a background
  watcher (`spawn_copilot_session_watcher`, 1s poll, 90s budget) looks for a session absent
  from that baseline (`newest_new_copilot_session`). It prefers a session whose recorded `cwd`
  matches the pane's — disambiguating agents spawned concurrently in different worktrees — and
  falls back to the newest fresh session. The `&self` method reaches a background thread via a
  stored `Weak<OrchRegistry>` self-handle (`set_self_arc`), avoiding a self-referential `Arc`.
- **Association.** On discovery, `associate_copilot_session` binds the id to the live pane: the
  agent map (so `list_agents`/resume see it), the durable roster (`agents.json`, which drives
  the session browser and restore), and any task-board item the agent owns. The roster write
  upgrades the pane's spawn-time placeholder (session `None`) in place rather than duplicating
  it. Audited as `copilot-session` (or `copilot-session-untracked` on timeout). The whole path
  honors `COPILOT_HOME`, matching the folder-trust writer, so it is fixture-testable.
- **Parity for free.** Once the id lands on the roster, everything Claude already had works for
  Copilot unchanged: `spawn_agent(resume_session, cwd)` (`--resume <id>`; ids are hex+dashes so
  they pass `sanitize_session`), session-browser restore (`resume_recorded_session`), and the
  ORCH/W/REV chips (derived from `session_roles()`).

Limitation: two Copilot agents started in the *same* cwd at the same instant can't be told
apart by cwd; the newest-session fallback may then bind the wrong one. Distinct worktrees (the
norm for parallel work) avoid this. A Copilot CLI that never writes session-state within 90s is
left untracked (audited), and can still be resumed manually from the session browser once it
does appear.

## Group lifecycle (#8)

Teardown used to mean ✕-clicking panes one at a time. A **group lifecycle panel**
(orchestrator pane header, Alt+O, `GroupView`) collects the whole-group controls in one
overlay — same no-resize overlay mechanics as the git / task / audit views — and sits
alongside the task board and #7's cost figures.

- **Group summary line.** `group_summary` / `orch_group_summary` reports the live-agent
  count, the role breakdown (orch / worker / reviewer / planner), and uptime — per agent and for the
  group as a whole (measured from the earliest-started live agent, i.e. the orchestrator).
  Uptime needs a spawn timestamp, so `AgentEntry` carries `started_ms` (distinct from
  `idle_since_ms`, which is about idleness, not age). The panel polls it every 2s and shows
  each agent's role, name, state (working / ready / idle-for), uptime, and — joined from
  #7's `group_usage` — its session cost, with the group total on the summary line.
- **End orchestration.** `end_group` / `orch_end_group` kills *every* agent in the group,
  the orchestrator included (unlike `kill_agent`, which protects it). It is deliberately a
  Tauri command only — **never** an MCP tool — so it is always human-initiated; the panel
  arms a two-click confirm before firing (destructive, irreversible). The teardown is
  audited as actor `human` (`group-end`, with the killed ids and worktree outcome). An
  optional **remove-worktrees** checkbox additionally reclaims each agent's worktree via
  `git worktree remove --force` (`worktree_cleanup_targets` picks the paths: deduped, and
  never the repo root — removing the user's own checkout would be catastrophic; the branch
  is always kept, only the working copy goes). Already-exited agents' worktrees are
  reclaimed too, since their roster entries still carry the path.
- **Closing the panes.** Killing a pty leaves a dead terminal pane open (agent panes are
  kept-on-error). So after the kill `end_group` emits `orch-group-ended`, which the
  frontend uses to close every pane in the group — the whole point of the action.
- **Composes with pause (#7).** Ending works regardless of pause (delivery suppression
  doesn't block a kill), and it clears the group's `paused` flag and marker file, so a
  later relaunch on the same repo id starts clean instead of silently resuming paused.
- **Spawn docking, on by default (#260).** New worker/reviewer/planner panes open
  straight into the minimize dock instead of expanding into the split tree, so a burst
  of delegate spawns doesn't crowd the orchestrator pane out of focus. Originally this
  reused #46's minimize/restore plumbing verbatim — `Grid.openPane` (full tree slot) then
  `Grid.minimize(pane)` once the pty finished spawning — so a freshly-docked pane behaved
  exactly like one a human folded by hand a moment after opening it. That "open, then
  fold" order painted at least one full-size frame during the pty spawn's IPC round trip
  (a visible flash, #387) and resized the pty to a layout size the pane was never shown
  at. `Grid.openPaneMinimized` (grid.ts) replaces it for the minimized path: the pane
  never gets a tree leaf at all, so its terminal opens detached and `fit()` lands on
  xterm's construction default (80×24) instead of any real slot — `restore()` still does
  the one genuine fit when a human actually reveals it. It still honors the "never dock
  the grid's last visible pane" guard (falls back to a normal, visible open when the grid
  has no other pane yet — the same edge case `Grid.minimize` itself no-ops on).
  A per-group **Auto-dock** toggle in the panel (mirrors the Notify toggle's shape:
  `spawn_expanded`/`set_spawn_expanded`, durable `spawn_expanded` marker, `orch_spawn_expanded`/
  `orch_set_spawn_expanded` commands) opts back into the pre-#260 always-expand behavior.
  The pure `spawn_opens_minimized(role, group_opted_expanded)` decision — `false` for the
  orchestrator unconditionally, `true` for every other role unless the group opted out —
  is called from both `SpawnRequest` construction sites (the orchestrator's own spawn and
  every delegate spawn), so the exemption can't drift between them. The orchestrator's own
  pane and any human-initiated open (launching an orchestrator, resuming an orchestrator
  session from the browser) are unaffected — those never go through `spawn_agent_ex`'s
  delegate path. One consequence worth knowing: a human manually resuming a single
  worker/reviewer session from the session browser *does* go through that same
  `spawn_agent_ex` path (`resume_recorded_session`'s worker/reviewer branch), so it
  inherits the docked default too — intentional, not an oversight; the Auto-dock toggle is
  the escape hatch for anyone who wants those to open expanded again.

## Stalled-agent watchdog (#10)

Silent-agent recovery used to live only in the orchestrator's prompt ("if a spawned agent
stays quiet, `get_output` and re-send"). That is best-effort: a busy or distracted
orchestrator can leave a wedged worker — one whose kickoff was eaten by the boot race, or
that is blocked on an input prompt — burning a pane indefinitely. Loomux already has the
primitives to automate the nudge, so the watchdog does, while leaving the *judgment* (what
to actually do) with the orchestrator.

- **What counts as stalled.** A *working* agent (running worker/reviewer with a task
  assigned, i.e. `idle_since_ms` clear) that has produced **no terminal output and sent no
  report** for the group's `watchdog_stall_minutes`. Output is read from the pty's monotonic
  byte counter (`PtyManager::output_total`, the same counter kickoff-readiness uses), which
  keeps growing even when the output ring saturates — so "did the CLI emit anything since
  last tick?" is a cheap integer compare. Silence is measured from `AgentEntry.last_progress_ms`,
  stamped at spawn and on every activity.
- **Reuses #7's plumbing.** A background loop (`start_watchdog`, 30s tick, mirrors
  `start_idle_reaper`) calls `run_watchdog`, which reads every pane's `output_total`
  (`agent_output_totals`) and hands the snapshot to `watchdog_tick`. Splitting the pty read
  from the decision keeps the stall / anti-nag / pause logic pure and fixture-testable with
  synthetic counters (no threads, no real pane) — the same shape as `reap_idle_agents`.
  The threshold arithmetic is the pure `watchdog_should_notify`; the config knob rides the
  existing `Guardrails` path (collected by the launcher, 0 = off, clamped in `clamped()`,
  persisted in `group.json`). Default **on** (10 min) — unlike idle-kill it is non-destructive.
- **The action.** One typed, audited (`watchdog-stall`) `[loomux]` notice is delivered to the
  orchestrator (`deliver_to_orchestrator`, actor `loomux`) naming the agent and suggesting
  `get_output` + re-send of the kickoff. It is advice, not an action: loomux never touches the
  wedged pane itself.
- **Anti-nag: one notice per stall.** `AgentEntry.watchdog_notified` latches when a notice
  fires and is *cleared* on any fresh sign of life — output growth (seen in `watchdog_tick`),
  a `report` (via `set_agent_idle(false)`'s re-arm), or a `message_orchestrator`
  (`note_agent_activity`). So a genuinely stuck agent is nudged once; one that moves again and
  re-stalls earns a new nudge. Output growth also resets `last_progress_ms`, so the clock only
  ever measures *uninterrupted* silence.
- **Interactions.** A **paused** group (#7) is skipped wholesale: delivery is suppressed there
  anyway, and — the subtle part — we must not spend the one-notice budget while paused, so the
  latch is left untouched and the outstanding stall still earns its first notice on resume
  (regression-tested). **Dead/reaped** agents (idle-kill or exit) are `Dead`/idle and thus
  outside the working-agent filter by construction, so a terminated pane is never flagged. The
  orchestrator is never watchdogged (it is the recipient).

## Delivery feedback loop (#103)

The watchdog catches an agent that goes wholly silent; this closes the tighter loop where a
single prompt *lands in the box but never submits* and the orchestrator, having gotten an
immediate-success `send_prompt` result (delivery is async), carries on none the wiser. It
rides #99's per-delivery `submit_confirmed` signal (the pane going quiet then bursting as the
box clears) rather than making the orchestrator poll terminals by hand.

- **The trigger.** When a delivery thread finishes with `confirmed == false`, it calls
  `notify_unconfirmed_delivery` off the outcome it recorded. The gate is the pure
  `should_notify_unconfirmed(target_is_orchestrator, confirmed)`: notify only for an
  unconfirmed delivery to a **non-orchestrator** agent.
- **The action.** One audited (`delivery-unconfirmed-notice`) `[loomux]` notice
  (`unconfirmed_delivery_notice`) to the orchestrator (`deliver_to_orchestrator`, actor
  `loomux`) naming the agent and pointing at the recovery move — `get_output` the pane,
  re-send if the prompt is stuck. Advice, not an action: loomux never re-types into the pane.
- **No loops.** A notice about a delivery *to the orchestrator* would itself be a delivery to
  the orchestrator — endless. So orchestrator-target deliveries never notify; they get #99's
  stranded-text flush on the next delivery instead.
- **One notice per delivery.** The emission sits past the submit retries, at the single tail
  of the delivery thread, so retries never multiply it — the analogue of the watchdog's
  once-per-stall latch, but scoped to the one delivery rather than a re-arming clock.
- **Interactions.** A **paused** group is skipped wholesale (same reasoning as the watchdog:
  delivery is suppressed there anyway, so we don't spend the notice). The template's
  Silent-agent recovery adds the human-facing half: on a repeat unconfirmed notice for the
  same agent, stop re-sending and flag the human.

## Prompt-landed signal (#112)

`submit_confirmed` (above, #103) trusts ANY output burst >= `SUBMIT_CONFIRM_MIN_BYTES` within
`SUBMIT_CONFIRM_WINDOW` after Enter as evidence the prompt landed. That heuristic is wrong in
BOTH directions, not just the one its name suggests:

- **False confirm.** Error repaints, dialog interactions, and spinner/statusline ticks all clear
  the byte bar. Two live failures recorded this way: a prompt merged with human-typed `/model`
  (#111) and swallowed as `Unknown command: /modelRun ...` — the error repaint after Enter
  exceeded the threshold, so the task was destroyed but recorded confirmed; and an open `/model`
  picker dialog similarly absorbed a delivery with no unconfirmed notice.
- **False unconfirm.** The confirm loop only ever runs `while reached_quiet && ...` — a BUSY pane
  that never reaches quiet before `SUBMIT_MAX_WAIT` skips confirmation entirely. Field evidence:
  one orchestration session drew "delivery unconfirmed" notices on 4 of 5 spawns, and in all four
  the prompt had actually landed and the agent was already executing — the notice's prescribed
  recovery (`get_output` the pane) costs a multi-thousand-token ANSI-garbled dump on every false
  positive (giving back exactly the spend #398 went to lengths to cut), and a signal that cries
  wolf trains the orchestrator to stop checking, which is precisely when the one true positive
  arrives.

Both failures share one root cause: there is no AUTHORITATIVE signal for what happened to a
pasted prompt, only an inference over the output byte stream — and #430/#432's ConPTY/xterm
cursor desync garbles that stream further. No retuning of `SUBMIT_CONFIRM_MIN_BYTES` or the
window fixes this; the axis itself is wrong (the same conclusion #420's design note reached about
byte-count baselines for the interactive-question guard). The fix is a real signal, reusing the
#417 hook seam this module already trusts for compact-lifecycle evidence.

### The docs facts (per the `agent-cli-reference` skill)

- **Claude Code hooks reference** (code.claude.com/docs/en/hooks, "UserPromptSubmit input"
  section, fetched and grepped directly against the raw page — round 1 review caught an earlier
  draft of this note citing `user_input` as the documented field, which does not appear anywhere
  on the page): `UserPromptSubmit` fires "when you submit a prompt, before Claude processes it",
  supports **no matcher** (always fires on every submission), and the stdin JSON payload carries
  the submitted text under **`prompt`** — verbatim, "UserPromptSubmit hooks receive the `prompt`
  field containing the text the user submitted." `user_input`/`user_prompt` are tolerated as
  legacy/cross-CLI fallback field names only, never treated as primary. **Safety-critical**: exit
  code 2 "blocks prompt processing and erases the prompt", and on exit 0 anything printed to
  **stdout is added as context Claude can see**. Hook commands receive input "piped via stdin as
  JSON" (confirmed separately, "How Hook Commands Receive Input"). Default timeout for this event
  is 30s.
- **Copilot hooks reference** (docs.github.com/en/copilot/reference/hooks-reference):
  `userPromptSubmitted` fires when "The user submits a prompt"; documented field names are
  `prompt` (camelCase: `sessionId`/`timestamp`/`cwd`/`prompt`) or `prompt` again under the
  VS-Code-compatible shape (`hook_event_name`/`session_id`/`timestamp`/`cwd`/`prompt`). The
  event's row in the page's own events table reads "Output processed: **No**" (re-verified
  directly against the raw page, not the rendered summary — the table has an `Event` / `Fires
  when` / `Output processed` / `Cloud agent` header row), so there is no exit-code-erases-the-
  prompt hazard on this side the way there is for Claude.
- **Docs-silent residuals** (named, not papered over):
  - **Copilot's payload TRANSPORT** for `userPromptSubmitted` is not documented — unlike
    Claude's explicit "stdin as JSON" contract, the Copilot reference gives field names but never
    says how the command hook receives them. Rather than guess (stdin? env vars? a file?), the
    Copilot arm never attempts to read the payload at all — see "Two confirmation tiers" below.
  - Whether a submission that gets swallowed/misparsed as an unknown slash command (the exact
    `/model`-merge failure this issue opened on) still fires `UserPromptSubmit` at all is not
    stated by either reference. If it doesn't fire, that case stays **unconfirmed** under this
    design — the safe direction (rev-32's own "never false-confirm" property, preserved) — left
    genuinely unresolved rather than assumed either way.

### Reuse, not invention: one more event on the #417 hook seam

Loomux already provisions per-CLI lifecycle hooks and polls marker files for `PreCompact`/
`SessionStart` (#417, "compact hooks as a trusted evidence source" below) — this adds ONE more
event to that existing machinery:

- `COMPACT_HOOK_SCRIPT` gains a `promptsubmit` arm on the SAME generic, per-machine `sh` script
  (`compact-hook.sh` — the filename still says "compact" even though it now also carries this
  event; renaming it would strand any live session whose already-generated `--settings`/hooks
  file points at the old name, so it stays, with a comment explaining why).
- `compact_hook_settings` gains a `UserPromptSubmit` entry in Claude's `--settings` hooks JSON —
  no `matcher` key, since the event doesn't support one.
- `ensure_copilot_compact_hook` gains a `userPromptSubmitted` entry in the SAME global
  `loomux-compact.json`, alongside `preCompact` — still one small additive file, same "all hook
  entries from all sources are run" guarantee.
- Marker-reading precedent: a new `<agent-id>.promptsubmit.jsonl` sits alongside the existing
  `.precompact.json`/`.sessionstart-compact.json` markers in the same `hooks/` dir.

### Two confirmation tiers, because the two CLIs' docs don't give the same guarantee

`PromptSubmitRecord { text: Option<String> }` is the parsed shape of one JSONL line.

- **Claude (content tier).** The script's `promptsubmit` arm appends the FULL stdin JSON verbatim
  (plus a trailing newline) to the marker. `promptsubmit_records_since(content, offset)` parses
  each line, pulling text from `prompt` (the documented field — see "The docs facts" above) or
  `user_input`/`user_prompt` (legacy/cross-CLI fallback only, never primary) — an unparseable
  line (a torn write mid-`>>`, racing a poll) degrades to `text: None`
  rather than being dropped, so a torn read never silently loses a whole poll cycle's evidence.
  `prompt_landed(records, pasted)` normalizes both sides (trim + collapse all whitespace runs,
  which also washes out CRLF-vs-LF) and does **containment**: a record whose text CONTAINS the
  normalized paste counts as landed, `merged: true` when it's a strict superset — the exact
  "prompt merged with human-typed `/model`" shape from this issue's root cause, still counted as
  landed (the agent DID receive the task text) but flagged in the audit (`confirm_merged`) rather
  than reported as a clean exact match.
- **Copilot (existence tier).** Since the payload transport isn't documented, the Copilot command
  never attempts to read or capture the prompt text — it appends a single non-JSON marker
  character (`.`) to the SAME marker file. `promptsubmit_records_since` treats any non-empty,
  non-JSON line as an existence record (`text: None`), so both CLIs share one reader; Copilot
  just never reaches the `Content` tier, only `PromptLandedMatch::Existence`. Strictly better
  than the burst heuristic for a busy pane (independent of `reached_quiet` — see below), at the
  cost of the content-match precision Claude's tier gets.

### The baseline: why a stale record can never satisfy a later delivery

`deliver_prompt` snapshots `promptsubmit_marker_len(path)` — the marker's byte length — right
BEFORE it pastes anything (not before the Enter; before the paste). `promptsubmit_records_since`
only looks at bytes after that offset. A record from an EARLIER delivery to the same pane, or a
human's own submitted prompt, sits entirely before this delivery's own baseline and can never
satisfy it — by construction, not by a timing assumption.

**Residual, named by round 1 review (N2), not hardened against**: the script's append is two
separate writes (`{ cat; printf '\n'; } >> marker` is one shell redirection, but the underlying
`write(2)` calls for the JSON body and the trailing newline are not atomic against a concurrent
reader). `promptsubmit_marker_len` could theoretically snapshot mid-append of a PRE-baseline
record; the remnant then parses, post-offset, as a non-empty non-JSON tail line — `text: None` —
which resolves to `PromptLandedMatch::Existence` and confirms this delivery on the strength of an
older record's own leftover tail. The window is microscopic (a delivery's baseline snapshot would
have to land in the handful of microseconds between two writes belonging to a DIFFERENT firing),
and only ever promotes to the weaker existence tier (a torn Claude record can't produce a false
CONTENT match, since the containment check needs a complete, valid JSON `prompt` value) — so it's
recorded here as a known, accepted gap rather than fixed with an offset-vs-last-newline check that
would add complexity for a hazard this narrow.

### The precedence, and the property that actually fixes the reported bug

`resolve_submit_confirmation(hook_match, reached_quiet, baseline_total, observed_total)` is pure:

```
hook_match != None            => (true, ConfirmSource::Hook)
submit_confirmed(reached_quiet, baseline_total, observed_total)
                               => (true, ConfirmSource::Burst)
otherwise                     => (false, ConfirmSource::None)
```

The load-bearing property: **hook confirmation is checked first and is NOT gated on
`reached_quiet`.** `submit_confirmed` (the burst fallback) keeps its exact existing shape and
gating, unchanged — nothing about its own false-confirm posture changes. `deliver_prompt`'s
confirm window now polls the hook marker on every tick regardless of `reached_quiet` (previously
the whole loop body — burst included — was skipped outright when the pane never reached quiet);
between the two spaced retries it polls again and breaks out of the retry loop immediately on a
match, on the theory that a hook match proves the ORIGINAL Enter worked (some CLIs can take
longer than `SUBMIT_CONFIRM_WINDOW` to emit the hook under load — exactly the busy-pane case this
feature exists for) rather than risk a redundant Enter blind-selecting an option in an unrelated
dialog that appeared since. `confirm_source` (`"hook"` / `"burst"` / `"none"`) and
`confirm_merged` ride the `prompt-typed` audit event so the two failure directions stay
distinguishable after the fact.

**The final re-check (added round 1 review, B3 — plan-14 step 4 had specified it and the first
push omitted it).** Each retry's own hook poll can be followed by `wait_for_question_clear`, which
holds for up to `QUESTION_HOLD_MAX` (120s). A hook record landing during that hold — or fired by
the LAST retry's own Enter, after the loop's own last poll of it — is invisible to every poll that
already ran. Left unchecked, `confirmed` stays `false` and the unconfirmed notice fires with the
proof already sitting on disk, unread: precisely the false-unconfirm class this feature exists to
erase, surviving on the one path the plan named to close. `apply_final_hook_recheck` (pure) runs
once more, right before `DeliveryOutcome` is recorded and `notify_unconfirmed_delivery` is called
— a no-op when already confirmed (never re-litigates a decision already made) or when the hook has
nothing new to say.

### Both-directions scorecard

- **False confirm**: on the Claude CONTENT tier, requires a hook record after the delivery's own
  baseline whose text CONTAINS the pasted text — an output burst, a dialog repaint, or a spinner
  tick writes no such record. **Residual, named by round 1 review (N1)**: the Copilot EXISTENCE
  tier has no content check at all — ANY post-baseline record confirms, so a human submitting
  their own prompt into that same pane during the confirm window or a retry's sleep (a span of
  several seconds) would false-confirm loomux's delivery. This is the precision cost the "Two
  confirmation tiers" section already names; it is real and unmitigated on Copilot, not merely a
  theoretical residual the way the Claude tier's is. The burst tier itself is untouched, so its
  own (narrow) false-confirm exposure is unchanged, not widened.
- **False unconfirm**: for a hook-provisioned CLI, a landed prompt fires the hook deterministically
  per the docs' own "always fires" contract, independent of the pane's output-quiet timing — the
  busy-pane class from the field-evidence session disappears. A CLI outside the hook seam (or a
  session where `resolve_hook_sh` found no `sh`) keeps today's behavior exactly — a degrade, not
  a regression.
- **Failure containment**: a hook that never fires (old CLI build, a `disableAllHooks` policy, a
  marker-write failure) silently degrades to the pre-existing burst tier — same fail-open posture
  #417 already established for the compact markers.

### Safety property: exit 0, print nothing, on every path

Claude's `UserPromptSubmit` is the highest-stakes hook this module has ever wired: getting the
exit code wrong doesn't just skip a signal (as a `PreCompact` marker-write failure would) — exit
code 2 **erases the user's own prompt**, and exit 0 leaks whatever the script prints to stdout
into the model's own context. The `promptsubmit` arm is held to a stricter bar than
`precompact`/`sessionstart-compact`: it `touch`es the marker first (an ORDINARY command failure a
non-interactive shell reports and continues past, per the SAME reasoning that already replaced a
bare `: > "$path"` redirect with `touch` for the other two events — see "#417: compact hooks as a
trusted evidence source" below) and only appends through `>> "$marker"` once that proves the path
is openable; on a `touch` failure it drains stdin to `/dev/null` instead of leaving it unread.
Either way, the append's target is the marker file or `/dev/null` — never the script's own
inherited stdout. Pinned by real-execution tests (the induced-failure `mkdir` case from #417's
own harness, extended to this arm, plus a happy-path test proving two firings append rather than
overwrite) with MUTATION evidence: a version of this arm that leaks stdin to stdout (a single
stray `cat` ahead of the touch-gate — a realistic typo, not a contrived one) fails the "no
stdout" assertion on BOTH real-execution tests, and a version of `resolve_submit_confirmation`
that re-gates the hook tier on `reached_quiet` fails the busy-pane pin — both mutations were run
and reverted; see the PR body for the exact commands and failure lines. **Honesty note (round 1
review corrected an inaccurate claim here)**: the `touch`-gate itself is defense-in-depth, not a
mutation-demonstrated regression catch on the `sh` this repo's tests actually run under — verified
directly, POSIX's fatal-on-redirection-error rule applies to *special built-ins* (a bare `: >`,
which the PRE-EXISTING `compact_hook_script...` test already pins) and not to ordinary utilities
or compound groups, so `{ cat; printf '\n'; } >> "$marker"` alone was confirmed NOT to abort this
script under the same induced `mkdir` failure. The gate is kept anyway for consistency with this
module's established style and because that POSIX distinction cannot be relied on across every
`sh` implementation this script might run under — but the claim that a mutation of THIS PR's own
construct demonstrates the gate catching a live regression was false and has been removed from
the code comment that used to make it (`COMPACT_HOOK_SCRIPT`'s doc).

### What this does NOT change (the #445 seam)

This PR touches the *confirmation* signal and its two provisioning surfaces only. It does not
widen, narrow, or re-time any `PasteGate::Abort` path, and `should_notify_unconfirmed` /
`should_notify_paste_held` (their orchestrator-suppression semantics included) are byte-identical
to before. `DeliveryOutcome`/`DeliveryConfirmation` keep their existing two-field shape — only
the audit event gains new keys. Payload lifecycle when a delivery genuinely can't proceed (queue-
don't-drop, flush-on-unblock, `report()`'s truthfulness) is #445's separate, not-yet-authorized
workstream; #112 hands it a trustworthy `confirmed` primitive to build on rather than widening
its own scope to cover it.

### Alternatives considered

- **Input-box-cleared check** on the ANSI-stripped tail: TUI rendering is not a documented
  contract (the references specify hooks, not paint); per-CLI box framing is exactly what
  breaks self-echo masking elsewhere in this module (see the paste-guard sections below); #430-
  class desync corrupts the read; and "box cleared" can't distinguish submitted from Ctrl-C'd.
  **Overturned in round 2 — see "Round 2: acceptance vs. processing" below.** The documented-
  contract advantage this rejection leaned on did not, in the end, save the hook: the live
  validation that follows found the docs silent on exactly the case that mattered (mid-turn/
  queued input), so "documented" was never the safety property it looked like. The Ctrl-C
  objection is closed for the human-cancel case: `tier1_trusted` (a pure predicate,
  `last_user_input_ms(pty_id) <= submit_sent_ms`) gates every point Tier 1's reading can become a
  decision — both Box confirms in the confirm/retry loop and the end-of-window veto
  (`final_window_outcome`) — so any human input since our own submit suppresses Tier 1 entirely
  for the rest of that delivery, in both directions. (Round 3, rev-20 B3, found this claim
  written here with no code behind it yet — the fourth claim-mismatch this PR produced. The
  fix above is what makes the sentence true; it wasn't at the time it was first written.) The
  submitted-vs-rejected ambiguity is NOT closed — it's named as a live residual in round 2's own
  section, made measurable rather than solved. Overturning a considered rejection without
  recording why is how a codebase loses its memory; this note is that record.
- **Echo check** (the pasted text appears in scrollback as a user message): same rendering-
  dependence, and long pastes get collapsed/elided by TUIs in undocumented ways. **This
  specific residual — long pastes collapsing under undocumented CLI rules — resurfaced almost
  verbatim as a live finding in round 2**, this time against Tier 1's own box-consumption check,
  not an echo check. The mechanism is different (tail-end containment, not scrollback presence)
  but the underlying hazard this bullet named turned out to be exactly right.
- **Turn-start signature** (spinner/thinking markers): per-version, per-CLI, localization-
  fragile, and doesn't distinguish a real turn from an error repaint's own spinner tick.
- **Transcript-file watching** (Claude's `transcript_path`): a real signal, but poll-based,
  session-file-format-coupled, Claude-only, and strictly dominated by the push-based hook the
  same vendor documents for exactly this purpose.
- **Retuning `SUBMIT_CONFIRM_MIN_BYTES`/the window**: cannot fix both directions at once — the
  axis (output bytes) is the wrong axis, not merely mis-calibrated.

### Round 2: acceptance vs. processing — why the hook-tier design above doesn't fix #112, and what does

Live validation of the design above (round 1, PR #451) found the mechanism sound but its central
claim false: across two real orchestration groups, **zero deliveries resolved `confirm_source:
"hook"`**. The hook fires and its records are correct (verified: one agent's marker file held a
byte-for-byte match for 4 of its own real deliveries) — but every one of those still resolved via
burst, and the two genuinely busy-pane cases resolved `"none"` (a false alarm) with no hook
record at all, even after 52+ seconds.

**Root cause 1 — a race the hook cannot win.** The confirm loop accepts whichever tier answers
first. Burst's bar (`reached_quiet` already true, plus any >=24-byte growth) is met by the TUI's
own synchronous repaint; the hook needs an OS process spawn, measured directly on the validation
machine at 87-98ms per invocation (and Claude Code's own docs show its hook command nests inside
Claude's OWN `sh -c` wrapper, so the real path is likely slower). Burst wins essentially every
healthy delivery by construction, not by accident.

**Root cause 2, and the one that actually reframes the fix — a logic constraint, not a tuning
problem.** `confirmed = hook_match OR burst_match` can only ever ADD confirmations; it has no
mechanism for the hook's ABSENCE of evidence to veto a burst false-positive. And per the live
data, hook silence carries no information: it's silent for the majority of provably-successful
deliveries (a `.promptsubmit.jsonl` growing from 5 to 28 lines over 38 minutes on one busy pane
that resolved every delivery via burst; frozen at 1-2 lines for 38+ minutes on another pane that
was genuinely working the whole time). So the false-CONFIRM direction — #112's own title — was
never reachable by this design at ANY hook coverage rate, hook-required is off the table (it would
convert every silently-successful delivery into a false unconfirmed — worse than the original
bug), and the false-UNCONFIRMED direction only degrades from "wrong at 45 seconds" to "wrong,
period" for a pane that stays busy indefinitely.

#### The actual defect: treating "no evidence yet" as "probably lost"

Claude Code accepts input while working and processes it later (observed directly, mid-session:
a delivered message appeared in the pane as `[Pasted text #5 +32 lines]  paste again to expand` /
`Press up to edit queued messages` — accepted and queued, not lost, while loomux had already
fired an unconfirmed alarm for it). A queued prompt produces no PROCESSING evidence — no burst,
no hook — for an arbitrarily long time, and that is normal, correct CLI behavior, not a fault. No
signal fixes that, because there is nothing to observe yet. **The fix is semantic, not another
oracle**: three states, not a confirmed/unconfirmed binary.

- `Confirmed` — positive evidence of acceptance.
- `Pending` — no evidence yet. The normal state for a prompt queued into a busy pane, for however
  long that takes. Never draws the unconfirmed notice.
- `Failed` — positive evidence of non-delivery, or a pane that's gone genuinely idle with none.

#### Tier 1: box consumption — two-sided, not another OR term

`echoed` (the pre-existing paste-verification check) establishes that SOME output appeared after
the paste — a raw >=8-byte growth check (`ECHO_MIN_BYTES`), no content comparison at all. The
first draft of Tier 1 assumed this meant "our literal text is positively in the box"; it doesn't,
and confirming that assumption is what surfaced root cause 2's sibling for Tier 1 (below). Once a
delivery's OWN precondition is separately, observably verified (see "The precondition problem"),
Tier 1 is genuinely two-sided in a way the hook never was: after Enter, our KNOWN pasted text is
either still findable at the tail end of the pane's output, or it isn't.

- **Gone from the tail end** (not "gone from anywhere" — a CLI that echoes an accepted prompt
  into scrollback/transcript history would make "anywhere" trivially true forever and prove
  nothing) → the CLI took it, whether it's processing now or queued. `ConfirmSource::Box`.
- **Still there, sustained across the WHOLE confirm+retry window (~7.6s across 15+ polls), never
  just one early sample** → a real veto: the CLI never even accepted the input.
  `ConfirmSource::BoxVeto`. The "never one sample" qualifier is load-bearing: render/redraw can
  lag Enter being processed by tens of milliseconds even on a healthy accept, so a veto that fired
  on the FIRST "still there" reading would misfire on ordinary redraw lag. The existing
  window's natural width supplies the debounce; no new TIME constant was needed for it — though
  round 3, below, adds a precondition on HOW the window ended (naturally, not via an early exit)
  that round 2's own first pass omitted.

This is what makes Tier 1 worth building where the hook wasn't: burst never decides when Tier 1
governs (its evidence is too weak to arbitrate against a box-based read — though it is still
evaluated every tick, purely to record `tier3_burst_would_confirm` in the audit trail), so a
healthy delivery is decided by the SAME signal in both directions, not raced against a weaker
fallback that can only ever say yes.

#### The precondition problem: `echoed` is a proxy, and Claude Code collapses long pastes

Live evidence (the same "Pasted text #5" episode above) showed the actual box, for a long
delivered prompt, NEVER contains the literal text at all — Claude Code collapses it to a
placeholder. `echoed`'s byte-count check is satisfied identically by the placeholder, so treating
`echoed=true` as proof of literal presence was the SAME category of mistake as trusting the
hook's silence: an assumption standing in for an observation, caught the SAME way, by a human
directly inspecting a live pane rather than trusting the code's own premise. The placeholder's
exact size/line threshold is genuinely undocumented (checked `interactive-mode`, the CLI's own
input-mode reference — no mention).

**The fix**: verify the precondition per delivery instead of assuming it. Right before Enter,
`deliver_prompt` reads the real tail and checks whether the pasted text is ACTUALLY findable
there (`box_holds_paste` — the same function serves this precondition check and the post-Enter
consumption check). Found → `tier1_governs = true`, Tier 1 decides two-sidedly as above. Not
found (a long/collapsed paste, or the echo genuinely never landed) → Tier 1 declines to govern,
audited explicitly (`tier1_governed: false`, and since #559 `tier1_decline` says which of the two
below), and this delivery falls back to the round-1 hook-or-burst precedence for its initial
window. **Consequence, stated plainly**: Tier 1's two-sided coverage is literal-echo pastes only.
A paste the CLI collapses to a placeholder — the orchestrator briefs most likely to land on a busy
worker and get queued, the exact population this redesign is for — doesn't get Tier 1's fast veto.
It still gets everything else in this design (the three-state model, the long-window late hook,
the idle trigger below), just not Tier 1's speed. **#559 narrowed that consequence**: it used to
read "short pastes only", because a *second*, unrelated limit — the size of the tail read — also
excluded every large paste whether the CLI collapsed it or not. That one is gone; see "The scan
window was the other half of the precondition", below.

Two options were weighed and set aside for this pass: a placeholder-pattern matcher
(`[Pasted text #N`/`Press up to edit queued messages`) would extend Tier 1's coverage to long
pastes, but it's undocumented, Claude-only, version-fragile TUI chrome taken on as a
CONFIRMATION source — exactly the class this design note already rejected once, and a format
change in a future Claude release would produce silent WRONG confirmations, not a visible
failure. Not built. Transcript-file watching (`transcript_path`, handed to every hook payload) is
a genuinely durable, hook-independent signal, but it's a new file format loomux doesn't parse
anywhere today; deferred as a follow-up if live measurement later shows the hook has coverage
gaps distinct from timing.

#### The scan window was the other half of the precondition (#559)

`box_holds_paste` asks whether our pasted text is in the last N bytes of the pane's output. Until
#559, N was `BOX_TAIL_SCAN_BYTES` — a flat 4 KiB, defined as `QUESTION_SCAN_TAIL_BYTES` because
"how much tail matters" seemed like one question. It is not one question. `QUEUE_FLUSH_MAX_BYTES`
lets a single coalesced flush paste 24 KiB, and **containment cannot hold when the haystack is
smaller than the needle**: for any paste past ~4 KiB the answer was `false` as a matter of
arithmetic, decided before the pane was ever consulted.

That is not a tuning miss, it is a category error, and it is why the fix is not a bigger number.
Two constants chosen for unrelated reasons — a token budget on one side, a per-poll scan cost on
the other — were jointly deciding whether a stranded delivery was ever noticed, and nothing in
either one's definition said so.

**The primary fix: say which `false` this is.** `BoxReading` replaces the bare bool at every Tier 1
consumer:

| Reading | What it establishes | Reached when |
|---|---|---|
| `Holds` | our text is identifiably at the box's tail end | the paste is found |
| `NotHolding` | an OBSERVED absence — the CLI consumed it, collapsed it, or it never landed | the tail was long enough to have contained the paste, and does not |
| `Unverifiable` | nothing at all | no read (pty gone), or the tail we got back is shorter than our own paste |

The three consumers that used to read a foregone `false` as an observation now each take the
reading:

- `unconfirmed_disposition` — `Unverifiable` is `Notify`, never `IdleAuditOnly`. #522's quiet path
  exists for a pane loomux *watched* go idle; a reading that never happened is not that pane. This
  is the silence #559 is about: an oversized stranded batch got no notice, no self-heal and no
  badge, and the ledger's own unconfirmed state was the only trace.
- `stranded_selfheal_action` — its own `StrandedBlocker::Unverifiable`, not `NotHolding`. Both
  refuse to press Enter, so this widens nothing about when loomux writes to a pane; what it stops
  is the badge *claiming* the human's text is gone on the strength of arithmetic. (`stranded_
  reword` treats it exactly like `NotHolding` for #517's in-flight-redelivery case, and #517's
  kickoff recovery triggers on either — the box reading was never that recovery's guard,
  `output_since_submit` is.)
- the confirm arm — `ConfirmSource::Box` now requires `NotHolding`. This is the direction that
  matters most: the old code's `Some(_)` arm would have taken a structurally-foregone `false` as
  proof the CLI accepted our paste, and a false confirm lets a stranded batch merge into the next
  delivery. Declining to govern is a cost; confirming something nobody observed is a defect.

Every decline is now stated where it happens rather than inferred from an absent `true`:
`prompt-typed` carries `tier1_decline` (`null` | `"not-holding"` | `"unverifiable"` — the same
`BoxReading::as_str` tokens every other record of a reading uses) with
`tier1_paste_bytes`/`tier1_scan_bytes` beside it, and the late monitor writes
`delivery-unconfirmed-box-unverifiable` whenever it reaches that reading — including on deliveries
that would have been notified anyway, so "Tier 1 was blind here" is never a fact only reconstructable
from what is missing.

**The secondary fix: derive the read from the paste, and bound it by the flush cap.** The scan
budget is no longer a shared constant; `tier1_scan_bytes` asks for the paste's own normalized
length plus `BOX_TAIL_SCAN_BYTES` of headroom for the framing/prompt/cursor chrome around it,
ceilinged at `TIER1_SCAN_MAX_BYTES = QUEUE_FLUSH_MAX_BYTES + BOX_TAIL_SCAN_BYTES`.

Three arguments for this direction rather than the other one the issue floated (capping the flush
at what a fixed scan can see):

1. **It widens nothing semantically.** `box_holds_paste` already windowed containment to the last
   *paste-length-plus-slack* of the normalized tail. The comparison was always paste-relative; only
   the READ was fixed, so it was truncating the window the comparison already used. A paste of
   4 KiB or less asks for exactly what it always did — the ordinary delivery does not move at all.
2. **It does not fight the feature it constrains.** Capping the flush at the scan window would have
   shrunk coalescing by 6x to protect a number that was never about coalescing (#533's whole point
   is fewer, batched deliveries).
3. **The cost is proportional to a paste we already paid for**, and only for deliveries that are
   themselves multi-KiB. `LATE_MONITOR_QUESTION_SCAN_BYTES` (32 KiB) is the standing precedent, and
   its own rationale already contemplates "Tier 1 governing a multi-KB brief" — a state that until
   #559 could not actually occur.

**Why `Unverifiable` is not dead code once the read is derived.** The sizing is best-effort, not a
guarantee, and deliberately so. A heavily-ANSI-escaped tail strips down to far fewer characters
than the bytes read; a pane whose ring has not accumulated enough output yet returns a short tail
whatever we ask for; a single queue entry larger than the flush cap still delivers alone
(`plan_flush`'s "the cap is a CEILING, never a floor") and exceeds the ceiling by construction. In
all of those the read comes back too short and the honest answer is `Unverifiable`. **The scan
sizing is best-effort; the honesty is structural** — which is the right way round, because a
best-effort number that quietly reports its own failures as observations is exactly what #559
was.

One invariant the code depends on, stated so an edit cannot lose it silently: **a box read must
never be narrower than the read whose answer it is revisiting.** A precondition verified against a
wide tail and then polled against a narrow one would read the box as cleared on the first poll and
confirm a delivery nothing observed. Until #685 that was enforced by size equality — all five call
sites (precondition, confirm loop, retry loop, and the late monitor's two) computed the same
`tier1_scan_bytes` from the same `pasted_text`.

Since #685 it is enforced two ways at once, and the distinction matters to anyone editing here:

- **Within one `Tier1Scan`, reads are monotone.** Its floor starts at `tier1_scan_bytes` and only
  ever rises, so the confirm loop and the retry loop can never poll narrower than the precondition
  read they are revisiting. This is the case the invariant was written for, and it is now strictly
  stronger than equality.
- **There are two `Tier1Scan` instances, not one** — `deliver_now`'s (precondition, confirm loop,
  retry loop) and `run_late_confirmation_monitor`'s (its two arms). They are independent, on
  different threads, and the monitor's floor starts at `tier1_scan_bytes` again, so its first
  *request* can be narrower than a widened request `deliver_now` already made. What makes that safe
  is not the request size: **every read widens itself before anything classifies it**, so what a
  reading is decided from is a tail that covers the containment window whenever the pane's density
  allows it at all — and when it does not, the tail is shorter than the needle and `box_reading`
  returns `Unverifiable`, never `NotHolding`. A narrower first request costs a re-read, not a
  confirm.

The load-bearing property, stated plainly so it is the one an edit has to preserve: **what makes a
`NotHolding` trustworthy is that the read covered the window, not that it matched some earlier
read's byte count.** Size equality was a proxy for that, and a poor one — two byte-equal reads
taken seconds apart already saw different tails as the ring filled.

#### Measuring the residual, rather than tuning against a guess (#583)

#559 left one thing accepted rather than fixed: **the slack is counted in raw bytes, and spent
after stripping.** `tier1_scan_bytes` asks for `paste + BOX_TAIL_SCAN_BYTES` *raw* bytes, but
`box_reading` compares what `strip_ansi` + `normalize_prompt_text` leave behind, and a TUI's
repaint stream loses a lot of both — escape sequences to the stripper, box padding to the
whitespace collapse. So the read has to survive a retention rate nothing measured, and near the
flush cap there is very little of it to spend:

| Paste (normalized) | Read asked for | Breakeven retention — below this, `Unverifiable` |
|---|---|---|
| 512 chars | 4,608 B | 11% |
| 1 KiB | 5,120 B | 20% |
| 4 KiB | 8,192 B | 50% |
| 8 KiB | 12,288 B | 67% |
| 16 KiB | 20,480 B | 80% |
| 24 KiB (`QUEUE_FLUSH_MAX_BYTES`) | 28,672 B | **85%** |
| past `TIER1_SCAN_MAX_BYTES` | 28,672 B (ceilinged) | unreachable — see below |

The shape of the table is the concern, not any one row: the tolerance *shrinks* exactly as the
paste grows, so the deliveries that most need Tier 1 to govern are the ones whose read has least
room to be wrong. A 1 KiB prompt tolerates an 80% loss; a coalesced flush at the cap tolerates 14%.

**One regime needs no measurement at all.** The read is ceilinged at `TIER1_SCAN_MAX_BYTES`, and a
single queue entry larger than the flush cap still delivers alone (`plan_flush`'s "the cap is a
CEILING, never a floor"). Above that ceiling the window is smaller than the needle *before any tail
is read*, so the delivery is `Unverifiable` at 100% retention — a pane emitting pure text with not
one escape in it still cannot be verified. No slack tuning reaches that regime; only a different
ceiling would, which is a different decision from the one this measurement informs. It is pinned by
`a_paste_past_the_scan_ceiling_is_unverifiable_at_any_density`.

Everything below the ceiling turns on live retention, which is a property of a CLI's repaint stream
— not derivable, so #583 is instrumented rather than answered here.

**What is recorded.** `Tier1ScanCensus` measures the read the reading was decided from — the same
read, never a second one — and both records carry it as `tier1_scan_census`:

| Field | Meaning |
|---|---|
| `requested_bytes` | what the ring was asked for — since #685 the **final** request of a read that may have widened itself, not the `tier1_scan_bytes` floor it started from |
| `tail_bytes` | raw bytes the ring actually returned; `null` = no read at all (pty gone) |
| `tail_chars` | what those bytes came to after `strip_ansi` + `normalize_prompt_text` |
| `paste_chars` | the needle's own normalized length (**not** `tier1_paste_bytes`, which is raw) |
| `margin_chars` | `tail_chars - paste_chars`; **negative is exactly the `Unverifiable` arm** |
| `retained_pct` | `tail_chars * 100 / tail_bytes` — the measured density the slack must buy through |

Lengths only: the audit never carries pasted text, and nothing here changes that.

Two sites, deliberately:

- **`prompt-typed`**, beside `tier1_decline` — on **every** delivery, `Holds` included. The question
  is a distribution, and a sample selected on the outcome cannot answer it: the successes are where
  the remaining headroom shows, and "how close did the ones that worked run" is most of the signal.
- **`delivery-unconfirmed-box-unverifiable`**, the late monitor's own reading — sampled minutes
  later against a much fuller ring. A second, differently-timed sample of the same question rather
  than a duplicate of the first.

`margin_chars` deciding the same cliff `box_reading` does is pinned by
`the_census_margin_is_the_same_cliff_the_reading_decides` — if the audit's arithmetic could drift
from the code's, a live distribution would answer a different question convincingly, which is worse
than not measuring.

**One producer-side interaction, so a live log is not misread (#632/#638).** Every framing row of a
coalesced flush is now marker-led, which adds the marker's bytes per constituent to the paste
itself — so `paste_chars` (and with it `requested_bytes`) is a little larger for coalesced
deliveries than it was before that change. It needs no correction when reading the numbers: each
record's breakeven is computed from its OWN `paste_chars` and `requested_bytes`, so every row
carries its own bar rather than being compared against a table constant. Nor does it move anything
past the ceiling: `FLUSH_ITEM_OVERHEAD`'s per-item charge is unchanged and still bounds the
composed text at `QUEUE_FLUSH_MAX_BYTES`, so the only pastes above `TIER1_SCAN_MAX_BYTES` remain
the single oversized entries that were always there.

**How to read the numbers.** Against a live group's `audit.jsonl`:

```sh
# Outcome by paste size (4 KiB buckets) — the headline distribution.
jq -r 'select(.action=="prompt-typed") | .detail
       | [ (.tier1_scan_census.paste_chars/4096|floor), (.tier1_decline // "holds") ] | @tsv' \
  audit.jsonl | sort | uniq -c

# Retention, counting only reads that FILLED the window (see the confounder below).
jq -r 'select(.action=="prompt-typed") | .detail.tier1_scan_census
       | select(.tail_bytes == .requested_bytes)
       | [.paste_chars, .retained_pct, .margin_chars] | @tsv' audit.jsonl
```

The confounder to exclude first: `tail_bytes < requested_bytes` means the ring simply had not
accumulated that much output yet. That is a short *pane*, not a dense one, and `retained_pct` over
such a read describes the pane's whole history rather than its escape density. Four readings, and
what each one implies:

1. **Near-cap deliveries mostly `holds`, with `margin_chars` comfortably positive** — the residual is
   theoretical. Close #583; the constant stays.
2. **Mostly `unverifiable`, on FULL windows, with `retained_pct` under the breakeven above** — the
   slack is the problem, and the fix is sized from the measured low-percentile retention: either a
   proportional slack (ask for `paste / ρ` rather than `paste + K`) or strip before sizing and re-read.
   Both cost scan bytes per delivery, which is why neither is worth doing on a guess.
   **This is what the live run came back as, and #685 took the second option — see the next
   section.**
3. **`unverifiable` with `tail_bytes < requested_bytes`** — a short ring, not density. Widening the
   slack buys nothing; the pane had not spoken yet, and only waiting or re-reading would help.
4. **`paste_chars` past the ceiling** — the proven regime above. Read it separately or it will
   contaminate the retention distribution with reads that were foregone before they happened.

Nothing in this section decides anything at runtime — it is instrumentation, on purpose. The right
slack is a tuning decision this makes answerable; taking it now would be the same "two constants
chosen for unrelated reasons" move #559 was about, one number further along.

**Boundaries with the work in flight** (#636, #640, both in the audit/notice area at the time of
writing): this adds one nested field to two existing `json!` records and introduces no new record,
no new vocabulary, and no change to any existing field. `Tier1ScanCensus::to_json` is the single
place the shape is built, for the same reason `BoxReading::as_str` sits next to its enum — two
records of one fact in two vocabularies cannot be aggregated, and this record exists only to be
aggregated.

#### Sizing the window in post-strip characters (#685)

The measurement came back as reading 2. Over 400 live Tier 1 reads on beta4, retention ran ~50%,
and verification coverage collapsed with paste size: 58% `unverifiable` in the 1.5-2 KiB bucket and
**100% from 2.5 KiB up**. Zero false confirms in either regime — the residual was always the safe
direction — but from a couple of KiB up, `box_reading` was answering out of arithmetic and the pane
was never consulted. That is not verification; it is a decline wearing a reading's name.

**The unit was the bug.** The read asked for `paste_chars + BOX_TAIL_SCAN_BYTES` *raw bytes* and the
comparison spends *post-strip characters*, so at ~50% retention the read handed the comparison about
half the haystack it asked for — and because the slack is a constant, the shortfall grows with the
paste until it crosses the needle's own length.

So `Tier1Scan` counts the budget in the unit that is actually spent:

1. read at the `tier1_scan_bytes` floor — exactly what #559 asked for;
2. measure what survived `strip_ansi` + `normalize_prompt_text` **in this pane**;
3. if that is short of the containment window, re-request scaled by the retention this tail just
   demonstrated: `requested × target ÷ tail_chars`.

**Why measured rather than a constant.** A proportional slack (`paste / ρ`, the other option above)
needs one ρ for every CLI, and #583's whole finding is that density is a property of a *repaint
stream* — a constant would be right for one CLI and wrong for the next, and it would over-read on
every ordinary delivery to buy headroom the ordinary delivery never needed. Scaling by what the read
in hand actually returned needs no ρ at all.

**What bounds it:**

- **The target is the containment window**, `paste + BOX_TAIL_WINDOW_SLACK` — the last of the
  normalized tail `box_holds_paste` ever looks at, not the whole 4 KiB budget. An ordinary delivery
  clears it on the first read and never widens, so only a read that was truncating the comparison
  costs anything (`an_ordinary_delivery_still_takes_exactly_one_read`).
- **`TIER1_SCAN_MAX_BYTES` still caps the target**, so a paste past the ceiling stays `Unverifiable`
  at any density — the widening cannot raise a ceiling nobody decided to raise
  (`a_paste_past_the_ceiling_is_still_unverifiable_however_wide_the_ring_is`).
- **A read the ring under-fills ends the widening on the spot.** Fewer bytes back than asked for
  means the ring holds no more, so no wider request can add one. This is reading 3's short-*pane*
  regime, and #685 leaves it exactly where it was: honestly `Unverifiable`.
- **`TIER1_SCAN_WIDEN_ROUNDS`** bounds the re-reads regardless, and running out classifies the
  widest read taken rather than no read at all. The raw ceiling is `OUTPUT_RING_CAP` — the whole
  readable universe, derived rather than picked, and in practice far above where the scaled request
  lands.

**Why this cannot manufacture a confirm.** The hazard needs a read *narrower* than the one it is
revisiting — one that sees the box cut mid-paste, reads `NotHolding`, and confirms a delivery
nothing observed. Two things rule it out. The first request is `tier1_scan_bytes` unchanged and
widening only grows it, so **no read after this change is narrower than the read the same call site
took before it**; and within a `Tier1Scan` the floor only rises, so a poll is never narrower than
the precondition it revisits (see the invariant above for what that does and does not cover across
the two instances). `box_reading` is untouched; `Unverifiable` governs everywhere it did.

**The residual this does not close**, stated rather than left for a reader to find: a read landing
between `paste_chars` and `paste_chars + BOX_TAIL_WINDOW_SLACK` post-strip characters can still
return `NotHolding` off a tail truncated mid-paste, because `box_reading`'s own threshold is the
needle's length, not the window's. That band is pre-existing and #685 makes it strictly rarer by
driving reads to cover the whole window; narrowing it further means changing the *classifier*, which
is a separate decision.

**What is deliberately NOT decided here.** #685 also asks what verification should *claim* for a
paste larger than a whole coalesced flush — the regime above the ceiling, where no window sizing
reaches and the only options are a different ceiling or a different channel. That is a posture
question with a human's name on it, and this change does not pre-empt it: the ceiling is the same
number it was, and the same regime is still `Unverifiable`.

#### Tier 2: the hook, no longer bounded by the window

"Stop requiring it in-window" — a late `promptsubmit` match upgrades `Pending` (or corrects an
already-`Failed` delivery) whenever it arrives, checked on a poll bounded only by the pty staying
alive and (round 3, below) a generous lifetime cap — not the original ~7.6s window. If a `failed`
alarm had already fired for this delivery, the late match is announced as a CORRECTION
(`delivery_confirmed_late_notice`), not a second success notice — the orchestrator needs to know
the earlier alarm was wrong, not just that things are now fine.

#### The `failed` trigger: idle-without-evidence, not a timer

Once the normal window closes `Pending`, `deliver_prompt` spawns a SEPARATE, unlocked, long-lived
monitor thread (deliberately not a continuation of the delivery thread, which holds the per-pty
delivery mutex for its own bounded lifetime — holding that mutex for the 38+ minutes the live
episode needed would block every subsequent delivery to the same pane). It declares `Failed`
only once the pane's own output has been quiet for `PENDING_IDLE_QUIET` (60s — its own constant,
not `SUBMIT_QUIET`'s 1s, which is tuned for an unrelated "safe to press Enter" bar) — never on a
fixed timeout (round 3 adds a generous lifetime CAP to the monitor thread itself, below, but
hitting it makes the monitor give up silently — `Expired`, no notice, `Pending` stays whatever it
already was — it never manufactures a `Failed` verdict; only genuine idle-without-evidence does
that). **Named plainly, per this design's own standard**: this is ANOTHER
byte-count proxy, the same category as `echoed`. Every "is this pane doing something" signal in
this codebase reduces to `output_total` not growing (`watchdog_tick`'s stall detection is the
same check, at a similar timescale, already trusted at production scale). It's a materially safer
bet than `echoed` was for two reasons, not because it's a different kind of signal: (1) a
minutes-scale margin dwarfs any transient gap in a CLI's own streaming cadence, where `echoed`'s
failure bit at the exact granularity it operated at; (2) it's being asked to prove something byte
count CAN support ("this pane stopped producing output") rather than something it structurally
never could (content presence). The residual this doesn't close: idle-without-hook-evidence isn't
PROOF of non-delivery, only the absence of the strongest proof available — a delivery accepted
and fully processed by a CLI whose hook simply never fired for that one turn (a coverage gap
distinct from Finding 1's timing race) would still false-alarm here, just only after waiting out
the whole busy period, not at 45 seconds. Measurable via the same independent audit fields: a
`Failed` delivery no hook ever later corroborates, for the life of the pty, is a real signature
future analysis can look for. **Stated plainly, not left implicit**: on such a pane this `Failed`
verdict is PERMANENT — Tier 2's late-hook correction (below) is the only mechanism that can ever
retract a `Failed` verdict, and by definition it never arrives on a pane whose hook doesn't fire,
so the one hole in a feature whose whole promise is "an away human returns to a trustworthy
record" is exactly this: a false alarm, on a hook-less pane, that nothing will ever correct.

**The guard that makes this safe to ship**: a pane can be quiet because it's holding a question
FOR THE HUMAN (an `AskUserQuestion`, a permission prompt) — output stops, the pane sits still,
and by pure quiescence it looks idle, but the human may be away for hours and the prompt is
legitimately still pending. Firing `failed` there would be the exact false alarm this redesign
exists to eliminate, in the exact circumstance (the human frequently away) it's supposed to
protect — and worse than today's noisy alarm, because a three-state `failed` carries false
authority. The monitor reuses `prompt_wait_detected` (mod.rs — the SAME detector #420's
safety-critical paste guard already trusts in production, masked for our own paste via
`mask_own_paste` exactly like every other checkpoint in this module) alongside the quiet check: a
`Failed` transition requires quiet-for-threshold AND no question currently on screen. Its own
known gap, already documented in its own doc comment rather than discovered here: "a footer
wrapped across rows in a very narrow pane, or a localized/reworded footer, won't match." A pane
quiet UNDER a dialog is also byte-quiet the whole time it's up, so the quiet clock keeps running
underneath it by construction; the only change is refusing to ACT on that reading while the
dialog is visible. Once the human answers, the redraw that follows is itself fresh activity, so a
genuinely-idle-after-that pane re-earns its own quiet-for-threshold observation before the alarm
fires — correct behavior, not a bug, and consistent with never declaring `failed` off a single
sample anywhere in this design.

#### The #445 seam, round 2: additive, deliberately widened once, surgically

Round 1 kept the seam byte-identical. Round 2 crosses it once, deliberately: WHEN the unconfirmed
notice fires changes (deferred from "end of the ~7.6s window" to "declared `Failed`", which for a
`Pending` delivery may be much later). What does NOT change: `should_notify_unconfirmed` /
`should_notify_paste_held` (both functions, byte-identical — the notice is still gated by the
SAME predicate, just called at a different point in time) and every `PasteGate::Abort` path
(untouched). The only genuinely NEW seam surface is additive: `notify_delivery_confirmed_late`,
a new function/notice for the correction case round 1 never needed (round 1 always notified
immediately, so there was nothing to correct against a still-silent alarm).

#### Audit shape: every tier records its own reading, never just the winner

`prompt-typed` carries `confirm_state` (`confirmed`/`pending`/`failed`), `confirm_source` (which
tier decided, if any), and three independent per-tier fields — `tier1_governed`/
`tier1_still_holding_at_end` (round 3, rev-20 N3: `null` when Tier 1 never governed this delivery
at all, not `false` — "never measured" and "measured and cleared" are different facts, and a bare
bool would have conflated them), `tier2_hook_matched`, `tier3_burst_would_confirm` — computed
regardless of which tier actually decided. This is the hard requirement round 1's own outcome
argued for: a suite that pins pure functions proved nothing about whether the mechanism won any
races in practice, because nothing recorded what LOST. A live run can now compare, per delivery,
what every tier actually said — including a Tier-1-veto that burst would have disagreed with, or
a hook match that arrived just after burst already won — the exact comparison this whole
redesign exists to make possible for the next person who has to trust or distrust it.

### Round 3: the enforcement details rev-20 found wrong, and why they're pinned now

Round 2's live-validation review found the architecture sound (three-state model, the seam, the
audit fields, the round-2 overturn) but three blocking defects in how it was WIRED — the exact
"round 1 shipped unpinnable wiring and failed live" mistake this whole redesign was supposed to
have learned from, recurring one level down.

**B1 — the late monitor leaked threads and could clobber a newer delivery.** Nothing bounded
`run_late_confirmation_monitor`'s lifetime except a hook match or the pty closing — for any pane
without working hook coverage (not hypothetical: live validation measured ZERO hook resolutions
across two groups), a `Pending`-then-`Failed` delivery's monitor polled forever. Worse: on a late
hook match it wrote `last_delivery` unconditionally, so a stale monitor could overwrite a NEWER
delivery's outcome, and could match the RE-SEND's own hook record and announce "correction: no
re-send needed" — false, since the re-send is why anything landed. Fixed with one mechanism for
both: each tick, if `last_delivery[pty_id]`'s `submit_sent_ms` no longer matches this monitor's
own, a newer delivery has superseded it — exit immediately, writing and notifying nothing
(`late_monitor_tick`'s `Superseded` branch, checked before even the hook match). A generous
lifetime cap (`LATE_MONITOR_MAX_LIFETIME`, 4 hours — the live episode needed 38 minutes) bounds
the one monitor that's genuinely the pane's most recent unresolved delivery.

**B2 — the in-window veto was reachable through a question-pending or human-typing exit.** The
question-holding-pane guard (above) was correctly built into the LATE monitor's idle trigger, but
the retry loop's early exits for a live question or human typing fell straight through to the
end-of-window veto check, which didn't know they'd happened — so `BoxVeto` (and the `Failed`
notice) could fire while a dialog was on screen, exactly the polarity this whole redesign exists
to forbid, reachable through the front door. Fixed: `final_window_outcome` (pure) adds a
`window_exhausted_naturally` precondition — a veto requires the confirm+retry window ran to
completion with NO early exit, for a question, human typing, OR a failed retry write (the pty is
likely closing; the box's end state can't be trusted as evidence either). Any early exit leaves
the delivery `Pending`; the question-guarded late monitor decides from there, since unlike this
one-shot end-of-window check it re-observes the question state on every subsequent poll.

**B3 — the design note's Ctrl-C claim had no code behind it.** The round-2 overturn (above)
claimed the human-cancel case was closed via `last_user_input_ms` before any confirm-path code
actually consulted it — a human pressing Esc/Ctrl-C during the window would clear the box (or
leave it looking cleared) with nothing distinguishing that from OUR delivery being accepted,
yielding a false `Box` confirm. This is the fourth instance, across this PR's review history, of
a claim reading as authoritative while not matching the code behind it (a phantom mutation-test
citation, a hallucinated API field name, an audit action filed under the wrong name, and now
this) — worth naming as a pattern, not just fixing as an instance. Fixed: `tier1_trusted` (pure,
`last_user_input_ms(pty_id) <= submit_sent_ms`) gates every point Tier 1's reading can become a
decision — both in-loop `Box` promotions and the end-of-window veto — so any human input since
our own submit suppresses Tier 1 for the rest of that delivery, in both directions, making the
overturn's own sentence true rather than aspirational.

All three fixes are pure, pinned functions, not inline conditionals — `tier1_trusted`,
`final_window_outcome`, and `late_monitor_tick` (with a `MonitorAction` result type covering
`Superseded`/`Expired`/`Confirm`/`DeclareFailed`/`KeepWaiting`) — each with a dedicated polarity
test and, for the three properties that matter most (never-veto-off-an-early-exit, never-decide-
after-human-input, supersession-beats-even-a-hook-match), mutation evidence: each property's
guard was individually removed, the corresponding test reproduced red at the exact assertion, and
the code was restored.

Non-blocking findings addressed the same pass: the late monitor's own question scan now reads a
much larger tail (`LATE_MONITOR_QUESTION_SCAN_BYTES`, 32KB vs. the 4KB `QUESTION_SCAN_TAIL_BYTES`
the fast 250ms-cadence checkpoints use) — sized for a 5-second poll cadence, not a 250ms one, so a
dialog rendered above a long literally-rendered paste can't fall entirely outside the read; and
`tier1_still_holding_at_end`'s null-vs-false distinction (above). Left explicitly unaddressed:
the correction path's own hook-coverage dependency (a `Failed` verdict on a hook-less/zero-
coverage pane can never be corrected, since correction rides the same hook that never fires
there — the idle-trigger residual already named above inherits this, and is now doubly
unresolvable on such a pane; a real gap, but closing it needs the same transcript-watching or
placeholder-signal work already deferred, not a wiring fix) and `notify_delivery_confirmed_late`'s
own silent-suppression-leaves-no-audit-trace shape, which mirrors an existing shape in
`notify_delivery_held`/`notify_unconfirmed_delivery` and belongs to whatever workstream addresses
that pattern generally (#445's own future territory), not a one-off fix here.

### Supersession during a re-send's in-flight window (#454)

B1's supersession rule is right; its TRIGGER was late. A newer delivery claimed the pane only
by writing `last_delivery` at the END of its confirm window — under a second when its hook
confirms in-window, ~9s worst case — while the `promptsubmit` record its Enter produces exists
almost immediately. In that gap a stale monitor reads "still mine" from the ledger and matches
the NEWER delivery's record as its own: a misattributed `delivery-confirmed-late` row and,
if it had already declared `Failed`, a "no re-send needed" correction about the very re-send
that is the only reason anything landed. #451 deferred this as a narrowed residual (the
dangerous polarity — a correction arriving BEFORE a re-send and suppressing it — was already
gone), and #454 is where it gets closed.

**Re-derived on current main before being fixed**, since the machinery moved a lot in between
(#445's queue, #470/#487's unified admission, #496 PR-C's self-heal). Unified admission
serializes *deliveries* through one drainer per pane, but the late monitor is deliberately a
detached thread holding no delivery mutex (that is what lets it outlive a ~52s delivery), so
nothing about admission ordering constrains it. The race was still live. It is also wider than
"a re-send of the same text": `PromptLandedMatch::Existence` matches any record past the
monitor's baseline regardless of content, so on a Copilot pane ANY newer delivery's Enter is
enough.

**The fix is an ordering, in two halves, both required:**

- `record_inflight_delivery` claims the pane in the ledger immediately BEFORE `deliver_now`'s
  first Enter, so ledger claim ≺ Enter ≺ hook record.
- the monitor takes its ledger observation AFTER its hook read (`observe_ledger`), so any hook
  evidence a tick can act on is checked against a ledger state at least as new as the evidence.

Together they close the window rather than narrowing it: a record a tick can see implies the
claim already happened, so that tick's own ledger read sees supersession. With only the writer
half, the gap shrinks from a whole confirm window to one tick's own work — still reachable,
which is why "narrowed" was never "closed".

**Why not a separate in-flight map**, which is the shape #454 itself suggested. Two maps means
two locks means a torn observation — read the in-flight map, miss the newer delivery, then read
the ledger — which is precisely the fused-lock defect #496 PR-C's admission gate had to be fixed
for (rev-47 B1). The claim goes into the SAME map under the SAME lock, and `observe_ledger`
returns every fact one tick decides from as a single value, so the single-observation discipline
is the type's job rather than a convention. The in-flight claim reads as `confirmed: false` —
the conservative value, identical to what a `Pending` delivery already leaves — so a delivery
that dies mid-window still arms the next delivery's stranded flush.

**One consequence, handled rather than absorbed.** A stale monitor now exits while the newer
delivery is still in flight, so the `Superseded`-and-confirmed badge drop it used to perform a
tick or two later can no longer happen for a delivery that confirms IN-window (which spawns no
monitor of its own). `deliver_now` drops the badge itself on a `Confirmed` outcome — a better
home for it anyway, since a confirmed delivery is direct proof the pane is not wedged.

**Evidence.** `queue.rs`'s `supersession_race_property` searches every reachable interleaving of
the newer delivery's steps, a genuinely-late record for the old delivery, and the monitor's two
reads, with one knob per half of the fix. Fixed is clean; each single-knob mutation reddens; the
as-shipped shape reddens (119 counter-examples). Two liveness tests keep "never confirm
anything" from passing as a fix — a correct late confirm must stay reachable, including while a
newer delivery races. `tests/orchestration.rs` drives the real seams through #454's own
interleaving. Residual, stated as rev-19 n1 stated the same one: no test pins that `deliver_now`
CALLS the recorder at its one call site; the property model's writer-knob mutation is what
models that deletion.

## Decision-grade structured reports (#398)

Every worker/reviewer/planner `report(...)` lands in the orchestrator's context window, and for
most of them the orchestrator is only ever a router whose next action depends on one bit plus a
reference — "review done, changes requested, PR #N" or "CI green, ready for review, PR #N" — while
the free-text `summary` had no shape and no cap, so a 300-word paraphrase of a review that was
already posted to the PR was a normal report. Verbose inbound reports were the single biggest
avoidable drain on the orchestrator's context, and every unneeded paragraph brings the next
compaction closer (the same pressure #287/#328/#329's compact-nudge work manages from the other
side).

- **Structural enforcement over prose.** `report`'s legacy shape (`status: progress|done|blocked`,
  free-text `summary`, no cap) still works — nothing that called it before this shipped breaks —
  but is soft-deprecated: the role templates stop teaching it. The new shape is `outcome` (a
  five-value enum: `done`, `blocked`, `progress`, plus `approved`/`request_changes` for a
  reviewer's report — a superset of the legacy three, not a parallel vocabulary), `ref` (the
  PR/issue, e.g. `"#123"`), `detail_url` (the GitHub comment/PR where the FULL detail already
  lives), and `note` — hard-capped at `report::NOTE_CHAR_CAP` (500 **characters**, never split
  mid-codepoint) by `report::truncate_note`, which always appends a stated `[…truncated, N chars
  total — see detail_url]` marker rather than silently cutting text. A cap the tool enforces beats
  a guideline the template merely asks for.
- **`outcome` implies `status` when the caller omits it** (`report::status_for_outcome`): `done` →
  `done`, `blocked` → `blocked`, `progress` → `progress`, and both `approved`/`request_changes` →
  `done` — a reviewer's turn being over (idle-kill clock restarts, attention badge clears) is the
  same event whichever way the review came out. This is what lets a fully-structured report never
  need the legacy field at all; `mcp.rs`'s dispatch validates whichever of `status`/`outcome` is
  given (rejecting an unrecognized value in either vocabulary, never defaulting one) and requires
  at least one, and likewise requires at least one of `summary`/`note` for the text.
- **Artifact-first is the assumption this rests on.** The report is a *notification*, not the
  record — the role templates (all four) now say so explicitly in their tool-doc bullet: post the
  full detail to GitHub FIRST (a worker's PR body/comment, a reviewer's review body, a planner's
  issue comment — already close to universal practice via the existing `review_verdict`/PR flow),
  then `report` the pointer. `report::structured_notice` composes the delivered `[loomux] <agent>
  reports <outcome> (<ref>): <note> — see <detail_url>` line — decision-grade, and small enough
  that even a batch of them doesn't threaten the next compaction.
- **Cutting the review fix-loop's middle hop.** The mirror change, in `orchestrator.md` and
  `worker.md`: on `request_changes`, the orchestrator routes **one line** to the worker ("review
  requested changes on PR #N — read the findings and revisit") rather than relaying the findings
  it never needed to hold in its own context — the findings are already on the PR (the reviewer
  posted them there before calling `report`), so the worker reads them directly. The full review
  never transits the orchestrator's context at all.
- **Template lockstep.** All four role templates changed; the `tests/fixtures/pre222` goldens were
  re-blessed in the same commit (see that directory's README for the diff-as-review-surface
  convention) — a prose pin (`tests/prompts.rs`) audited clean, since none of #398's edits touched
  a pinned region.

## Notification backend (#243)

Three MCP tools — `notify_when`, `list_notifications`, `cancel_notification` — let the
orchestrator, a worker, or a reviewer register a structured condition (a PR's CI checks, or
a `gh run` id) and get a `[loomux] …` notice (event-led, e.g. `[loomux] PR #241 checks:
SUCCESS — … (watch n-3)` — matching the house style of every other `[loomux]` notice, which
leads with what happened and names itself last) typed into their **own** pane the moment it
resolves, instead of sitting in a wait loop or re-polling `gh pr checks` on a cadence. The
`workflow_run` fail-cancel notice is the one deliberate exception to event-leading: "cancelled"
is also a legitimate GitHub run *conclusion*, so `"run 17812 cancelled after 3 failed polls"`
would read as the CI run itself being cancelled rather than as `gh` being unreachable three
times. That notice instead puts the watch id between the label and the verb — `[loomux] run
17812: watch n-5 cancelled after 3 failed polls — gh-not-found` — so the watch, not the run, is
what the sentence says got cancelled (rev-ui, PR #247 round 2). Not
available to a planner (see **Tool surface** above). The audit trail for all six lifecycle
events uses a `watch-*` action prefix (`watch-register`/`watch-fired`/`watch-expired`/
`watch-failed`/`watch-cancel`/`watch-cleanup`) — deliberately not `notify-*`, which the
group's pre-existing desktop-notification toggle already owns (`notify-on`/`notify-off`);
sharing a prefix in the one audit surface a human filters would have made two unrelated
features indistinguishable there (rev-ui, PR #247).

Human-visible surfacing of a live watch (issue #248, split from this PR because it's a
frontend feature larger than the backend it surfaces) — the group-view "⏳ waiting on …"
indicator, the audit's `watch-*` summarize() sentences, and the watchdog's "may be
deliberately waiting" annotation — reads the exact same `watches` registry state this section
describes; no second store.

### Fire-time facts vs the registration-time note (#531)

A watch's `note` is frozen the instant it is registered; the verdict it ends up attached to is
not. Observed three times on 2026-07-30: a note written while the branch was at `ccf191c` was
delivered alongside a checks verdict resolved at `a77c4d1`, because the branch had been
re-pushed while the watch was outstanding. Nothing in the notice said so, so a *current* result
arrived labelled with a *stale* SHA — the exact shape of "a green run on an old head is not a
merge license", and the reader had to go re-derive the head by hand before trusting its own
ready-state.

So the fired notice states the volatile facts as of FIRE time, alongside (never instead of) the
note:

- Each `pr_checks` poll reads `headRefOid` off the `gh pr view` mergeability pre-check it was
  already making (#337) — one process, two facts — and carries it on the tick's `Poll` next to
  the `PollResult`. The classification vocabulary is untouched; a head SHA is an observation
  riding with a verdict, not a kind of verdict.
- The notice names that head (`Head at this poll: a77c4d1`), and when it differs from the first
  head this watch ever observed, says so outright: `MOVED from ccf191c since this watch began;
  re-verify the head before acting on the note`. The head clause is emitted *before* the note so
  `NOTICE_TOTAL_CAP` truncation trims the note's tail rather than the freshness fact.
- The note is echoed verbatim as `Note (registered): "…"` — relabelled, never rewritten. The
  backend does not re-derive or reinterpret what the agent meant; it only fixes the *as-of*.

The baseline is the first head the **poller** observed, not the head at registration:
`register_notification` runs inside the agent's own `notify_when` call and shells out to
nothing, and putting a `gh` round-trip in front of every registration to buy a marginally
earlier baseline is the wrong trade. `due_watches` makes a never-polled watch immediately due,
so in practice the baseline lands within one `NOTIFY_POLL_INTERVAL` of registration. The
residue is deliberate and documented on `Watch::first_head`: a re-push inside that first window
becomes the baseline and is therefore invisible to the MOVED marker — which is precisely why the
notice always states the head it actually saw, instead of leaving the marker as the only
freshness signal. Every failure mode of the head read (a `gh` error, a missing field, a
non-hex value) degrades to *no head clause at all* and the pre-#531 wording: a wrong SHA in
front of an orchestrator is the one outcome worse than no SHA.

### Why structured kinds, not a caller-supplied poll command

The obvious generic shape — `notify_when(poll_command, predicate)` — was considered and
rejected, because it moves the trust boundary rather than automating inside it:

- Agent panes get `agent_pane_env()`'s shimmed `PATH` (see **Enforced merge gate** below),
  which is what makes `gh pr merge`/`gh release`/a `v*` tag push refuse an agent in its own
  pane. A poll command handed to the *registry* would run from the loomux app process, with
  the backend's real, **unshimmed** PATH — the gate an agent cannot get past in its own pane
  would not apply to a command string it handed to the poller. An agent that cannot merge its
  own PR could register `gh pr merge 241` as a "poll command" behind a predicate that never
  matches, and loomux would run it — as the user — every 30s, forever. The side effect *is*
  the payload; the predicate is decoration.
- It is also strictly more powerful than anything an agent can already do: a command it types
  into its own pane is visible, runs under the shims, and dies with the agent. A registered
  poll command is invisible in that sense (it runs on the poller thread, not in any pane),
  unshimmed, and **outlives the agent's turn** — repeating, unattended, until cancelled or
  expired. "Agents can already run `gh`" is not a license for that.
- It also contradicts CLAUDE.md constraint 6 (the backend trusts the webview, not agent
  input) and the add-orch-tool design norm ("guardrails in the platform, judgment in the
  prompt") — a caller-supplied command moves judgment about what's safe to run into the
  prompt, exactly backwards.

Structured kinds cost one small PR per new condition (`pr_merged`, `pr_comment`,
`review_verdict`, … are natural v2 follow-ups). That is the correct price: the backend owns
the whole `gh` argv, and the only agent-supplied bytes are a `u64` (a PR or run number) —
nothing agent-controlled ever reaches a command line as a string, and every predicate is a
pure function over pinned `--json` fields, testable with canned fixtures and no `gh`.

### Shape: mirrors the watchdog and `pr_head`, invents nothing new

- **Pure core** (`orchestration/notify.rs`, ~350 lines including tests): `Condition`
  (`PrChecks { pr }` | `WorkflowRun { run }` — no `Default`, so an unrecognized wire `kind`
  has nothing to fall back to and is rejected outright), `Watch`, `PollResult`
  (`Pending`/`Met`/`Failed`), the two predicates (`pr_checks_result`, `workflow_run_result`),
  the notice-text functions, and the cap/TTL/interval constants. Mirrors `workflow.rs` /
  `profiles.rs`: `mod.rs` is already ~9k lines, so a new pure-function-heavy feature gets its
  own file rather than growing it further.
- **The `gh` subprocess shape.** A private `OrchRegistry::gh_capture(repo, args)` resolves
  `gh` through `winpath::resolve_program` (a bare `Command::new("gh")` won't resolve a
  Windows `gh.cmd` shim-free) and pins `CREATE_NO_WINDOW`, mirroring the shape
  `write_shim`/`pr_head`-style helpers already use elsewhere in this file. **This lands as a
  fresh helper, not a lift of an existing `pr_head`**: at the time this PR was written,
  `main` had no `pr_head` — it exists only on the not-yet-merged `feat/222-custom-workflows`
  branch (user-defined agent workflows). A follow-up should fold `pr_head` into
  `gh_capture` once #222 merges, rather than keep two copies of the same subprocess shape
  permanently; noted here so it isn't lost. The wait itself is bounded rather than an
  `output()` — see "Bounding one tick (#656)" below.
- **The tick split** (the `watchdog_tick` shape, exactly): `poll_watches(&self)` is the
  impure half — shells out to `gh` for each id `notify::due_watches` selects, and classifies
  each result with the pure predicate. **The selection policy itself is pure**, not just the
  decision policy: `due_watches(now, &watches, &paused) -> Vec<String>` (in `notify.rs`) owns
  the per-watch 30s floor, the round-robin ordering by `last_poll_ms`, the
  `MAX_POLLS_PER_TICK` (8) cap, and the paused-skip — `poll_watches` is a thin wrapper that
  calls it and then shells out for whatever it returns. This was originally inline in
  `poll_watches` with zero coverage of the `gh`-process DoS backstop it implements (rev-tests,
  PR #247); lifting it is the same move `notify_tick` already makes for the decision half.
  `notify_tick(&self, now, &results)` is the decision half: pause/expiry/fail-streak/fire
  policy over an **injected** `now` and poll results, so **no test shells out to `gh`** —
  every test in `tests/orchestration.rs` drives `notify_tick` directly with a synthetic
  `PollResult` map, the same seam that makes `watchdog_tick` testable with synthetic pty
  counters. `run_gh_poll_tick` = `poll_watches(now)` + `gh_poll_tick(now, …)`, called every
  `NOTIFY_POLL_INTERVAL` (30s) by `start_gh_poller`, registered in `lib.rs` beside
  `start_watchdog`. (#406 renamed that glue from `run_notify_tick`/`start_notify_poller` when
  the intake scan moved onto this same tick — see "One `gh` poller" below. The split, the
  cadence and the seam every test drives are unchanged.)
- **Delivery** reuses `deliver_prompt(agent_id, text, "loomux", Delivery::MidSession)` — the
  same path the watchdog nudge, the idle-tick, and worker reports already use. No new side
  channel (add-orch-tool design norm): every existing guard comes free (per-pane serialized
  delivery, the pause suppression, the #111 human-typing hold, the #103 unconfirmed-delivery
  notice).

### Constants

| constant | value | why |
| --- | --- | --- |
| `NOTIFY_POLL_INTERVAL` | 30s | poller tick cadence, and the floor between polls of one watch |
| `MAX_POLLS_PER_TICK` | 8 | bounds `gh` process churn per tick regardless of board size |
| `MAX_WATCHES_PER_AGENT` | 4 | per-agent cap; a rejection names it |
| `MAX_WATCHES_PER_GROUP` | 12 | per-group cap; a rejection names it (independently of the per-agent cap) |
| `NOTIFY_EXPIRES_DEFAULT_MIN` / `_MIN` / `_MAX` | 60 / 5 / 240 | TTL default and clamp (`Guardrails::clamped` idiom — never reject a plausible number, never trust it unclamped) |
| `NOTIFY_FAIL_STREAK_LIMIT` | 3 | consecutive `gh` failures (auth, `gh-not-found`, unknown PR/run) before the watch is cancelled rather than polled forever against nothing |
| `GH_CAPTURE_TIMEOUT` | 20s | bound on ONE `gh` child; a stalled subprocess would otherwise park the single poll loop and every notice with it (#656 — see "Bounding one tick") |
| `MAX_INTAKE_POLLS_PER_TICK` | 4 | groups the intake half polls per scan — 2 `gh` calls each, so the same 8-call budget as `MAX_POLLS_PER_TICK` (#656) |
| `GH_CAPTURE_REAP_TIMEOUT` | 2s | bounds the post-kill reap, so no arm of the bounded wait is itself unbounded (#656) |
| `GH_CAPTURE_MAX_LEAKED_READERS` | 16 | process-wide ceiling on readers abandoned by timed-out captures; past it a capture is refused rather than spawned (#656) |

### Predicates and the "no checks reported" trap

`pr_checks` polls `gh pr checks <pr> --json state,name,link`; met when the array is
**non-empty and none of `PENDING`/`QUEUED`/`IN_PROGRESS`**. `gh pr checks` exits **non-zero**
with "no checks reported on the '\<branch\>' branch" on a just-pushed PR — orchestrator.md
already warned that checks take a minute to appear, and this predicate maps that exit to
**`Pending`, never `Met`/`Failed`**. Getting this backwards fires an instant, wrong SUCCESS
the moment a PR opens, before CI has even registered a check — costly enough (and easy
enough to get wrong) that it has its own pinned regression test. `workflow_run` polls
`gh run view <id> --json status,conclusion`; met when `status == "completed"`, and the
notice carries `conclusion`.

Among the terminal (non-pending) rows, `SUCCESS`, `SKIPPED`, and `NEUTRAL` all count as
**non-failing** — GitHub's own branch protection ignores `SKIPPED`/`NEUTRAL` when deciding
mergeability, and a condition-gated job (e.g. a `deploy` step that only runs on `push`)
reports `SKIPPED` on every PR event, not `SUCCESS`. Treating anything-not-`SUCCESS` as
failing (the original implementation) fired a false "FAILURE — N of M checks failed" the
moment the release-pipeline change added such a job to every PR run (rev-orch, #290). Any
other terminal state — including one `gh` hasn't documented yet — stays classified as
failing: an unrecognized conclusion must never silently read as passing. When every check is
non-failing but at least one was skipped, the summary keeps the skip visible rather than
folding it into a bare "all passed" (`SUCCESS — 4 of 5 checks passed (1 skipped)`).

### Caps, expiry, pause, and agent death

- **Caps** are checked at registration, independently: an agent under its own cap can still
  be rejected for the group cap, and vice versa (both are tested).
- **Expiry** always speaks: a watch past its deadline is dropped and its owner gets a
  `[loomux] … expired after N min … (watch n-3)` notice naming the manual fallback
  (`gh pr checks <n>` / `gh run view <id>`) — silent expiry is the one failure mode that
  stranded an agent forever, so it never happens quietly. The `N` reported is
  `Watch::nominal_ttl_ms` (fixed at registration), never a recomputation from `deadline_ms` —
  see the pause note below for why those two numbers must not be the same field.
- **A paused group freezes the TTL clock, not just the expiry check.** `deadline_ms` is an
  *absolute* wall-clock timestamp, so skipping the expiry check while paused is not enough on
  its own: real time keeps passing underneath it, and the first tick after a long pause would
  find every outstanding watch already past its (unmoved) deadline — evaporating exactly the
  watches the freeze exists to protect. This shipped broken in the first version of this PR
  (rev-orch, PR #247 round 1, with a repro) and is fixed by `notify_tick` maintaining
  `paused_watch_since: HashMap<group, tick_time>`, reconciled against **the current `paused`
  set itself**, not "groups that currently hold a watch": every group in `paused` not already
  recorded gets `paused_watch_since[group] = now`; every group recorded but no longer in
  `paused` (it resumed) has its span computed once and its record cleared. This bookkeeping
  deliberately lives in `notify_tick`, not in `pause_group`/`resume_group`: those two use real
  wall-clock `now_ms()` directly (they are Tauri-command-reachable, unrelated to the notify
  subsystem, and changing their signature to accept an injectable `now` would be a wider API
  change than this fix warrants), which a test's simulated `now` can never reach — so the
  freeze has to be reconstructed from the `now` values `notify_tick` is actually called with.
  In production this lags true pause/resume by at most one `NOTIFY_POLL_INTERVAL`
  (`start_gh_poller` ticks every group regardless of its pause state).

  **Two round-2 defects in this mechanism, both from the same root cause** (rev-orch, PR #247
  round 2, with reproducing probes): the span is computed *per group* but was being applied to
  *every* watch in it with no regard for that watch's own lifetime.
  - **B1 — a stale entry outlives the group emptying out.** Scanning "groups that currently
    hold a watch" (rather than `paused` itself) meant a group that lost every watch while
    paused — its one worker idle-killed, cancelled, or crashed, all routine, all funnel through
    `mark_dead` — dropped out of the scan entirely. No later tick could even see the group to
    reconcile it, so the entry sat stranded, unreconciled, straight through the resume, until
    some completely unrelated LATER watch registered into that (long-since-resumed) group —
    which then inherited the whole stale span. **Fixed** by reconciling against `paused`
    directly (above), which cannot go stale: it is re-derived from the live pause state every
    single tick, with or without a watch present.
  - **B2 — a watch registered mid-pause is charged time it never lived through.** Agent panes
    keep running while their group is paused (only prompt *delivery* is suppressed), so
    `notify_when` still works mid-pause. Applying the group's whole elapsed span to that watch
    charged it for the part of the pause that predates its own existence. **Fixed** by clamping
    each watch's credit to `(elapsed span).min(now - w.registered_ms)` — the span it actually
    lived through, never more.
  - Both fixes are independently necessary: the scan fix alone still lets a *live* watch in a
    stale-but-since-cleared group over-credit itself once (bounded by its own age at that
    point); the clamp alone bounds a single tick's damage but doesn't stop a group's stale entry
    from recurring across ticks. Regression-pinned in `tests/orchestration.rs`
    (`notify_stale_pause_entry_is_reconciled_even_while_its_group_has_no_watches`,
    `notify_watch_registered_mid_pause_is_credited_only_the_span_it_actually_lived_through`),
    each mutation-verified red against its own fix removed.
- **Agent death** (`mark_dead`, covering idle-kill, `kill_agent`, a crash, and the planner
  auto-close identically, since all four funnel through it) drops that agent's watches in
  one line, audited (`watch-cleanup`) only when something was actually removed. No delivery
  is attempted (the pane is gone) and no orchestrator notice is sent (the audit line is
  enough; a notice per dead agent's stranded watches would be noise).

### Persistence: in-memory only, deliberately

Watches are TTL-bounded (≤4h) and describe in-flight CI — not durable state in the sense
`state.json`/the task board/the PR itself are. Persisting them would mean rebinding an owner
across a restart where agent ids and panes are re-minted: real complexity for a case where
the durable record already survives and the orchestrator's session-start re-sync already
re-reads it. The cost this pushes onto the template: **on session start (and after a
compaction), call `list_notifications()` and re-register anything you were waiting on** —
`orchestrator.md`'s durability rules and `worker.md`'s tool bullet both say so. This is a
documented limitation, not an oversight.

### Known interactions (stated, not fixed here)

- **The #112 delivery weakness applies here too.** `submit_confirmed` false-confirms on any
  output burst, so a fired notice landing unsubmitted in an agent's input box can still be
  recorded as delivered. A watch is one-shot (dropped the instant it fires), so a lost notice
  is a missed wake — mitigated by (a) auditing `watch-fired` *before* delivery, so the
  run stays reconstructible, and (b) the orchestrator template keeping its PR-comment sweep
  as an explicit fallback rather than deleting it: a dropped notice degrades to the old
  poll-based behavior, not a hang (pinned in `tests/prompts.rs` — this is now the ONLY thing
  standing between a lost notice and a silent hang, which is exactly the kind of rule that
  suite exists to keep from quietly disappearing).
- **The watchdog does not know about notifications.** A worker parked waiting on a
  `pr_checks` watch is, correctly, producing no output and sending no report — exactly what
  `watchdog_should_notify` looks for. It will still trip the stall notice to the orchestrator
  after `watchdog_stall_minutes`. Acceptable for v1 (the notice is one line and already reads
  as "may be waiting on input"); teaching the watchdog about live watches is a follow-up, not
  a defect this PR needs to fix.
- **Self-addressed delivery is asserted by construction, not independently pinned by a
  test.** No notify tool takes an `agent_id` parameter, so there is no code path that could
  even name another agent as a delivery target — `deliver_prompt(&w.agent, …)` is the only
  call, and `w.agent` is set once, at registration, from the caller's own MCP-token identity.
  rev-orch (PR #247) tried to falsify this with a targeted mutation (hardcoding the delivery
  target to a fixed agent id) and it passed unnoticed: `deliver_prompt` isn't observable in
  the integration harness (agents have no pty in test mode, and the one audit line that fires
  *before* the pty-existence check only covers the paused-suppression branch, which a live
  notify delivery never takes — notify simply skips a paused group's watches outright rather
  than attempting delivery into one). Making this independently testable would mean adding a
  registry-wide "last `deliver_prompt` target" test seam touched by every caller of a
  widely-shared, delivery-critical function — real surface for one property that already has
  no code path to violate. Stated here rather than left as an unearned "tested" claim.
- **Security**: no new execution capability (the only subprocess is `gh`, backend-owned
  argv); no `group_id`-as-path-segment exposure (the poll cwd is resolved from the caller's
  **group**, which comes from the MCP token, never from an argument — constraint 6 is never
  engaged); every GitHub-derived string and the agent's own `note` is sanitized
  (`sanitize_gh_text`) before it enters a notice: control characters (including newlines) are
  stripped so an embedded newline can't forge a second `[loomux] …`-prefixed line that reads
  as its own, separate notice, AND `[`/`]` are mapped to `(`/`)` so the literal token
  `[loomux]` can't survive even mid-line (a fork PR names its own workflow jobs, so a check
  named `[loomux] all checks passed` is adversary-chosen text, not hypothetical — rev-orch,
  PR #247). `run` ids parse through a dedicated `run_id_from`, not the bare `pr_number`
  tail-digits parse: a job-linked run URL (`.../actions/runs/17812/job/98765`) would otherwise
  silently resolve to the *job* id instead of the run id (rev-orch, PR #247).

## Cross-workspace communication channels (#271)

Full design in `doc/design/cross-workspace-channel.md`; summarized here for the tool-surface
table's context. Two MCP tools, `channel_send(text)` / `channel_status()`, let an
orchestrator/worker/reviewer (not a planner — the same #243 exclusion) broadcast to and read
who's on the other end of a human-connected **channel**: a set of two-or-more agent panes,
possibly in **different orchestration groups** (a "workspace" is a project tab; loomux is one
process/one registry, so "cross-workspace" is cross-group inside it, not cross-process).
Connection itself is human-only — two Tauri commands
(`orch_channel_connect`/`orch_channel_disconnect`), never an MCP tool — so the trust boundary
constraint 6 usually protects (an agent cannot see another group) is relaxed only along edges
a human explicitly drew, and an agent can never widen it: `channel_send` takes no
group/agent-id argument, only `text`, sanitized with the same `sanitize_gh_text` (#243) every
other crossing-text boundary uses, with the sender identity built by loomux, never the agent.
State (`channels`/`agent_channel` maps) and delivery mirror `watches` exactly — in-memory
only, same `deliver_prompt(..., MidSession)` path, same audit-then-best-effort-deliver shape.
This PR ships the backend + MCP surface + typed frontend command wrappers; the pane
context-menu connect gesture and cross-tab chip UI are a stacked follow-up.

## Autonomous mode (#83)

The orchestrator template already documents a full idle cadence — poll `agent-ready`/
`agent-investigate` labels, groom them, re-check open PRs — "on the slow periodic cadence
while otherwise idle." But an LLM CLI only acts when text is typed into it, and **nothing in
the backend ever poked an idle orchestrator**: every wake-up (worker report, board change,
human message, watchdog stall, max-agents change) is event-driven. When a group went quiet
the cadence simply never ran. Autonomous mode closes that gap with a **tick source**, plus
the two cost/safety controls the unattended-spend risk demands.

- **Idle-tick loop.** `start_idle_tick` (60s wake, clone of `start_watchdog`) calls
  `run_idle_tick`, which reads each live orchestrator pane's `output_total` and
  `last_user_input_ms` (`orchestrator_activity`, the analogue of `agent_output_totals`) and
  hands the snapshot to `idle_tick_tick`. Splitting the pty read from the decision keeps the
  gate/latch/cap/pause logic pure and fixture-testable with synthetic maps — the
  `watchdog_tick` shape. An orchestrator output-quiet past `IDLE_TICK_MINUTES` (15, a fixed
  constant in v1) earns exactly one audited (`idle-tick`) `[loomux] idle tick` notice via
  `deliver_to_orchestrator` (mid-session delivery — the same #43-hardened paste path a live
  orchestrator receives any prompt through) telling it to run its cadence and **start** labeled
  work. The threshold arithmetic is the pure `idle_tick_should_fire`.
- **Window: 5 min default, per-group tunable.** `Guardrails.idle_tick_minutes` (default
  `DEFAULT_IDLE_TICK_MINUTES` = 5; 0 → default, floored at 1 — the `autonomous` marker, not
  this, is the on/off switch; persisted in group.json, live-settable via
  `set_idle_tick_minutes`). The original 15-min fixed constant was the root cause of a live
  test where an 8-minute autonomous session simply never fired; 5 min matches the human's
  "action within a few minutes" expectation, and the knob lets them drop to 1–2 min to verify.
- **Repaint-tolerant quiet signal.** `output_total` counts *every* byte, including
  statusline/spinner repaints that keep creeping while the CLI is parked — and there is no
  output-frame classifier (the #112 work classifies human *input*, not output). So treating
  *any* growth as activity (as the watchdog does) let a single stray repaint byte reset the
  whole quiet window, so an orchestrator that repaints even occasionally could never
  accumulate a full window and never ticked. The idle tick instead discriminates by size (pure
  `idle_output_is_activity`): only per-tick growth `>= idle_activity_floor_bytes` counts as the
  orchestrator working and resets the clock + latch; sub-floor growth rebaselines the counter but
  leaves the quiet clock running. So one repaint can never demand another full window of silence.
  The **default 2048** is justified by measurement — a captured full idle Claude Code input-box
  render (box-drawing + ANSI) is ~164 bytes (`tests/fixtures/attention/idle-input-box.txt`, pinned
  by a test), so 2048 gives ~12× headroom over a complete idle repaint. No raw idle-pane byte
  *stream* is captured anywhere and spawning a live CLI is forbidden, so that rendered-frame size
  is the honest available measurement. Because this rides the exact wake+spend axis that already
  failed once, the floor is a **live-tunable guardrail** (`Guardrails.idle_activity_floor_bytes`,
  0→default, clamped `1..=1 MiB`, persisted, audited, `set_idle_activity_floor`) — the runtime
  remedy if a chattier CLI's idle repaints exceed the default.
- **Self-regulating + capped.** A real output burst (the orchestrator acting) resets the quiet
  clock **and** clears the one-notice latch (`AgentEntry.idle_tick_notified`, mirroring
  `watchdog_notified`), so the worst case is one tick per idle window — an action defers the
  next tick, so it can't tight-loop. A hard `MAX_IDLE_TICKS_PER_HOUR` backstop (per-group
  timestamp ring, `idle_tick_times`, reusing `spawn_rate_exceeded`'s window rule) catches any
  pathological re-arm. Recent **human input** in the pane folds into the quiet clock too
  (belt-and-suspenders on top of output-silence), so a tick never lands while the human is
  steering. **Paused** groups are skipped wholesale and their latch left intact (same
  reasoning as the watchdog).
- **The input fold is bounded (#496).** Two independent heuristics key off the same "pane has
  recent human input" signal — this fold, and delivery's stranded-text-flush/submit-retry
  suppression — and both can deadlock if that signal is ever wrong and never clears on its own:
  a live copilot orchestrator wedged exactly this way when xterm's own automatic replies to the
  program's terminal queries (OSC colour, DA, DSR/CPR, focus reports) stamped
  `last_user_input_ms` with no human present, forever, so the tick that would have recovered the
  group could never fire — only a physical Enter did. `Guardrails.idle_tick_input_defer_max_minutes`
  (0 → default `DEFAULT_IDLE_TICK_INPUT_DEFER_MAX_MINUTES` = 15, i.e. 3x the tick window; floored
  at the group's own `idle_tick_minutes`, capped at 24h; no live setter this round, same
  precedent as `context_window_tokens_override` — hand-edit `group.json`) clamps how far input
  alone may push the quiet clock past `AgentEntry.last_output_progress_ms`, a NEW field tracking
  only output-based resets (never input-deferred ones) — so a signal that keeps refreshing every
  scan, forever, makes the tick fire late rather than never. This is deliberately a *separate*
  guarantee from the root-cause fix (gating `write_pty`'s stamp on `classify_human_input` so
  xterm's own auto-replies never reach `last_user_input_ms` at all — see `pty.rs`): the root-cause
  fix is correct for the mechanism that was actually observed; this bound is the backstop for any
  refresher nobody has modeled yet — a future IME/composition path, a different CLI's TUI,
  anything not yet seen. A tick that fires because of the bound (rather than the ordinary quiet
  window) is audited with its own `reason` (`idle-tick-input-defer-bound`) rather than reading as
  an unremarkable fire. **The clamp is classification-blind, deliberately** — a guarantee must not
  trust the very classifier it is backstopping — so it caps the raw timestamp regardless of what
  produced it: genuine, real (Content-classified) keystrokes are capped exactly the same as
  pure-Neutral traffic byte-indistinguishable from an xterm auto-reply (arrow keys, menu
  navigation). Sustained genuine typing with zero real output for the full 15 minutes is capped
  too, and the tick fires mid-typing; this is rare in practice (submitting produces a real output
  burst well inside the window) and where it isn't, the human-mid-work protection is delivery's
  OWN hold (`USER_QUIET_MAX_HOLD`, the #420 state-based question guard), not an exemption in this
  clamp — carving Content out here would re-open the exact deadlock class this bound closes.
  Fifteen minutes of zero real output *and* zero input at all — the common case this bound exists
  for — is either a wedge or a human who has walked away, and firing a NOTICE (not an action) is
  correct either way — `MAX_IDLE_TICKS_PER_HOUR` still backstops runaway firing regardless.
- **Observability.** Because the tick is otherwise invisible until it fires, `orch_autonomy`
  surfaces `idle_tick_minutes`, `idle_activity_floor_bytes`, and (while on) `quiet_secs`,
  `eligible_in_secs`, and `tick_status`. The countdown is **honest** (`idle_tick_observability`):
  `eligible_in_secs` is a real timer only for `counting_down` / `eligible` / `rate_capped`; when
  the one-notice latch gates the next tick (`waiting_for_activity`) there is no timer — it waits
  for the orchestrator to emit output — so `eligible_in_secs` is `null`, never a lying 0. The
  per-hour cap folds in as a real timer (time until the oldest tick ages out of the window). The
  computation mirrors every skip-gate `idle_tick_tick` applies so the panel can't show a live
  countdown while ticks are actually suppressed: `paused` (autonomous and paused are independent
  markers — a paused group suppresses all delivery) and `starting` (a still-booting orchestrator;
  the tick only considers Running panes) both report `null` countdown.
- **The toggle.** Off by default. `is_autonomous`/`set_autonomous` on the `set_notify`
  marker-file pattern (an `autonomous` marker), so it's live-togglable from the group panel
  and survives restarts (re-seeded in `create_group` next to `paused`/`notify`). The label
  funnel stays the consent boundary: autonomous mode starts *labeled* work on its own; it
  never triages unlabeled issues (option (c) of the investigation, rejected).
- **Cost guardrail — token budget.** The headline cost control. `Guardrails.autonomy_budget_tokens`
  (u64; 0 = no cap; persisted in group.json, live-settable via `set_autonomy_budget` like
  `max_agents`) caps **autonomous-era** spend. The anchor problem — budget lifetime history or
  only new spend? — is settled by metering the **delta from an enable-time snapshot**: enabling
  stamps the group's current `group_usage` token total into the `autonomous` marker's *content*
  (`autonomy_anchor`), and `enforce_autonomy_budgets` (run each cycle before the tick) meters
  `group_token_total(group) - anchor`. Crossing the budget (`autonomy_budget_exhausted`)
  **suspends** autonomous mode — flips the marker off (explicit consent required to resume),
  audits `autonomy-budget-exhausted`, and delivers **one** `[loomux]` notice; because
  suspension leaves the autonomous set, later passes skip the group so it can't repeat. The
  suspension also writes a durable `autonomy_suspended` marker (cleared on a genuine re-enable)
  so `orch_autonomy` can report `suspended: true` — the UI distinguishes a budget suspension
  from a plain user toggle-off without reconstructing it from the audit log. **The money-stop is
  unconditional:** unlike a *user* disable (disk-first + fail-loud, to protect the consent
  boundary — a failed removal keeps it ON), the suspension path (`suspend_autonomous`) drops the
  in-memory flag **regardless of whether the marker can be removed**, because continued spend
  past the cap is the one direction this feature must never allow. If the durable removal fails,
  the surviving `autonomous` marker is overridden at restart by the `autonomy_suspended` marker
  (the `create_group` re-seed checks suspended first), so the group comes back OFF +
  suspended-visible rather than silently ticking. This is
  genuinely **new enforcement** — exact per-session token accounting already existed
  (`usage.rs`, `group_usage`) but no spend cap did. Tokens, not dollars: subscription/Max
  accounts pay $0 marginal, so dollars are meaningless here (see `usage.rs`). Re-enabling
  re-anchors at the now-higher spend, which is what "toggle to resume" means.
- **Merge-approval toggle.** `is_auto_merge`/`set_auto_merge` (an `auto_merge` marker, default
  OFF = today's human merge gate). The *behavior* lives in the orchestrator template — its merge
  section is now conditional on the flag — and the backend just stores/exposes it and mirrors it
  into the orchestrator's context two ways: the kickoff prompt renders the current gate (for a
  fresh boot/resume) and a live toggle delivers an audited `[loomux] auto-merge …` notice (for
  the running orchestrator), exactly how `max_agents` surfaces (kickoff render + live notice).
  When enabled the orchestrator may merge an adequately-tested PR (reviewer-approved + green CI +
  acceptance met) itself, auditing and announcing each merge and still holding anything
  risky/ambiguous for the human.
- **Commands (frozen contract; W2 builds the UI against it).** `orch_set_autonomous(group_id,
  enabled)`, `orch_set_auto_merge(group_id, enabled)`, `orch_set_autonomy_budget(group_id,
  tokens) -> u64`, `orch_set_idle_tick_minutes(group_id, minutes) -> u32`, and
  `orch_autonomy(group_id) -> { autonomous, auto_merge, budget_tokens, budget_anchor_tokens,
  spend_since_enable_tokens, suspended, idle_tick_minutes, quiet_secs, eligible_in_secs }` — the
  one read the group panel renders all controls, the live budget meter, the budget-suspended
  state, and the idle-tick countdown from. Registered in `lib.rs` beside `orch_set_notify`.
- **This group could be affected.** The feature is generic — loomux's own orchestration group is
  just another group, so nothing special-cases it. Turning autonomous mode on for the group
  loomux is developed in would idle-tick *its* orchestrator like any other.
- **Interactions.** Idle-kill is unaffected: the orchestrator is never idle-reaped, and a tick
  delivered to it never touches worker `idle_since_ms`, so idle workers still reap on schedule.
  Spawns a tick induces still count against `max_spawns_per_hour`. The human's pause/off-switch
  is instant.

### Idle-tick intake gate (#332)

The idle tick above fires on a **fixed quiet-window schedule**, regardless of whether anything
has actually changed — the common case in a settled group is a full LLM turn against the
orchestrator's (largest) context that ends "nothing new, going quiet." The intake gate makes the
tick **conditional** on a host-side, zero-token pre-check for exactly the things the tick's
cadence exists to catch: new/changed `agent-ready`/`agent-investigation` labels, open-PR
check-state transitions, and (#864) new comment/review activity on an open PR.

- **A scan on the shared poller's tick** (#406; originally a second, independently-clocked
  thread of its own — see "One `gh` poller" below for why that was folded away).
  `gh_poll_tick` calls `poll_intake(now)` on the wakes where `intake_scan_due` says
  `INTAKE_POLL_SCAN_INTERVAL` (60s) has elapsed since the last scan, which — for
  every **autonomous** group whose EFFECTIVE `intake_poll_minutes`
  (`intake::effective_intake_poll_minutes` — #429's smart default, resolved fresh every scan) is
  nonzero and due (`intake::due_intake_polls`, the `notify::due_watches` per-group-interval
  idiom, plus that idiom's per-tick cap and oldest-polled-first ordering — see "Bounding one
  tick") — shells out
  to `gh issue list --json number,title,labels` and `gh pr list --json
  number,title,statusCheckRollup,comments,reviews` (via the existing `gh_capture` helper
  `poll_watches` already uses; no new subprocess plumbing) and diffs the result against that
  group's last-seen state
  (`OrchRegistry.intake_seen`, in-memory only — the `watches`/`idle_tick_times` lifetime class).
  Two calls per due group regardless of how many issues/PRs exist — the API-budget discipline
  `notify.rs`'s round-robin/per-tick cap defends, applied here as "one list call, not one per
  item."
- **Pure diff + notice composition (`intake.rs`, mirrors `notify.rs` exactly).**
  `label_deltas`/`pr_check_deltas`/`pr_comment_deltas` take the already-parsed `gh --json` output and the prior
  poll's last-seen maps and return only what's NEW: a label present now that wasn't at the last
  observation (covers both a brand-new labeled issue and a label added to a known one; a label
  removed then re-added reads as new again, not a repeat), or a PR whose coarse check-state
  (`PrCheckState::{Pending,Success,Failure}`, classified with the exact `check_is_pending`/
  `check_is_failing` predicates `notify::pr_checks_result` already uses, so the two call sites
  can never disagree about what "failing" means) reached a NEW terminal value, or (#864) a PR whose
  newest comment/review timestamp differs from the last observation — *differs*, not "is greater",
  because an edited or deleted comment moves it backwards and the question being asked is "has the
  discussion changed since loomux looked", so the failure direction is one spurious wake rather
  than a missed one — **with one deliberate exception: a PR going from some discussion to NONE is
  silent** (deleting the last comment leaves nothing to read, so the wake would carry no content;
  the stale last-seen entry it leaves cannot swallow a later post, since any new comment is newer
  than the deleted one and so still compares as different). Timestamps compare as strings:
  GitHub's are fixed-width UTC `Z`, so
  lexicographic order is chronological and no date crate (and so no `getrandom`-pulling
  dependency — CLAUDE.md constraint 2) is needed. Comment activity rides the PR call that was
  already being made, so the poller's two-`gh`-calls-per-due-group **round-trip** budget is
  unchanged — but the *bytes* are not: `gh` has no sub-field selection, so every comment and review
  body comes down whether or not anything reads it, measured at 20,034 → 288,284 bytes (14.4x) for
  this repo's 14 open PRs, growing linearly with discussion volume across the 30 most recent open
  PRs `gh` returns by default. Serde skips the bodies without allocating them, so the cost lands on
  one larger response string per poll rather than one per comment, and the read is uncapped, so no
  truncation path can silently drop the check-state half of the same response. It
  also cannot tell an agent's PR comment from a human's — every agent shares the human's `gh`
  credential — so an orchestrator that comments and then falls quiet is woken once for its own
  words; a deliberate over-approximation, bounded by the quiet window, still cheaper than the
  per-tick manual poll it replaces. A restart empties
  `intake_seen`, so the very next poll reads everything currently labeled/terminal as new exactly
  once — the acceptance criterion ("harmless one-time re-fire, never a repeat") falls out of the
  diff with no special-casing. `intake_wake_summary` composes the addendum text, sanitized with
  `notify::sanitize_gh_text` exactly like every other GitHub-derived field reaching a `[loomux]`
  notice (issue titles are third-party text — the #189 threat model applies here too), capped at
  `MAX_SIGNALS_IN_SUMMARY` with a stated "+N more" rather than growing unboundedly.
- **The gate itself (`intake::idle_tick_gate`).** Once `idle_tick_should_fire`'s quiet-window
  threshold clears (unchanged from #83), a group with the gate ON consults **four** signals
  before actually notifying: a pending intake signal from the poller above; an outstanding
  `notify_when` CI watch for the group (the "a lost notification degrades to poll-on-sweep"
  invariant already in `orchestrator.md` — an outstanding watch means the tick's fallback-sweep
  duty still has a job even with zero label/PR news); an unresolved watchdog stall
  (`AgentEntry.watchdog_notified` on any agent in the group); or the **bounded fallback**
  (`intake::idle_tick_fallback_due`, `Guardrails.idle_tick_fallback_minutes`, default 180 = 3h,
  the middle of the issue's suggested 2–4h range, widened while the group stays delta-free — see
  the #864 bullet below) having come due since the group's last actual
  fire (`OrchRegistry.idle_tick_last_fired_ms`) — never since the gate was last merely
  *evaluated*, which can happen every 60s scan while a group sits quiet. Any one of the four
  fires the tick; none of them SKIPS it — audited (`idle-tick-skipped`, with the reason), never
  silently. A skip re-arms the one-notice latch (`idle_tick_notified`) so the loop doesn't
  busy-spin re-checking every scan, but with a NEW per-agent field
  (`AgentEntry.idle_tick_skip_rearm_ms`) that auto-clears it once `intake_poll_minutes` could
  actually have refreshed the poller's findings — unlike the #83 latch, which only clears on the
  orchestrator producing real output, a *skip* means it produced none, so nothing else would ever
  clear it.
- **`intake_poll_minutes` is tri-state (`Option<u32>`), smart-defaulted ON while autonomous
  (#429)** — the same "let a real value distinguish from absence" idiom
  `compact_nudge_min_context_percent`/`context_window_tokens_override` already use in this
  struct, resolved fresh at gate-evaluation time (`intake::effective_intake_poll_minutes`), never
  baked into `Guardrails::clamped()`, so a group that flips autonomous mode ON gets the gate
  immediately with no re-launch. `None` (unset — absent from `group.json`, or never explicitly
  set) resolves to `DEFAULT_INTAKE_POLL_MINUTES` (5, matching `DEFAULT_IDLE_TICK_MINUTES` — the
  poller need not run more often than the tick it feeds) whenever the group is autonomous, and to
  `0` (inert) while supervised. `Some(0)` is an explicit, deliberate opt-out — the escape hatch
  for an operator who wants the polling load off even while autonomous, set by hand-editing
  `group.json` (there is no live setter for this field, same precedent as
  `context_window_tokens_override`). `Some(n)`, `n > 0`, is an explicit cadence, clamped to
  `1..=1440`. This is a deliberate REVERSAL of the field's original migration-safety stance (an
  upgraded group's pre-#429 `group.json`, with no such key, used to decode to `0`/off byte for
  byte) — see the #429 finding below for why. `idle_tick_fallback_minutes`, by contrast, still
  follows the normal idiom (0 → default, then clamped `30..=10080` min) — it is the backstop *for*
  the gate, so it must never be configurable to "off."
- **When the tick DOES fire because of the gate**, the notice (`idle_tick_notice`, now taking an
  `Option<&str>` intake summary) states what changed — issue numbers, PR state deltas — so the
  orchestrator can act on it directly instead of re-polling the exact thing loomux already
  checked for zero tokens. This is the same "don't make the recipient re-derive what the sender
  already knows" principle behind #398's structured reports, applied to the opposite direction of
  the same channel.
- **The fallback backs off while a group stays delta-free (#864).** The bounded fallback is what
  keeps a poller bug from muting the orchestrator — but at a *fixed* interval it also charged a
  group parked entirely on the human (every open item human-gated, no live delegates, nothing
  changing anywhere the host can see) that interval forever: ~30 consecutive delta-free wakes over
  one parked weekend, each 1–2 API turns over the orchestrator's whole ~180k-token prefix, each
  sweep finding nothing. So the EFFECTIVE interval `idle_tick_gate` consults is
  `intake::fallback_interval_minutes(base, streak, ceiling)`: the base doubles per consecutive
  delta-free wake and stops at `Guardrails.idle_tick_fallback_max_minutes` (default 24h → 3h → 6h
  → 12h → 24h). Three properties carry the design:
  - **The streak grows only on a delivered wake that found nothing** (`heartbeat`), never on a gate
    *evaluation*. A skip re-checks every `intake_poll_minutes`; ramping the interval on those would
    couple the cadence to the poll rate rather than to observed quiet.
  - **It resets on evidence about the world, not about the orchestrator**: a wake with a real signal
    behind it, human input in the pane since the last fire, or any live delegate in the group.
    Notably NOT on orchestrator output — in a parked group the dominant source of that output is the
    orchestrator answering our own previous wake (it sweeps, finds nothing, goes quiet), which is
    exactly the loop being slowed down. Resetting there would make the streak un-growable in
    production while still passing any test that never simulated the reply, so the integration tests
    simulate it deliberately.
  - **The guarantee changes in degree, not in kind.** The orchestrator is still woken
    unconditionally; the worst case is one wake per ceiling instead of one per base. Setting the
    ceiling equal to the base is the documented opt-out — one value, rather than a separate enable
    flag — and the ceiling is floored at the base in `clamped()`, so the backoff can only ever make
    the backstop slower, never quicker. The streak lives in `OrchRegistry.idle_tick_empty_streak`,
    in-memory like `idle_tick_times`: a restart drops back to the base cadence and has to re-earn
    the backoff by observing quiet again, which is the safe direction.
  `empty_streak` and the effective `fallback_minutes` ride every `idle-tick` audit entry and the
  `idle-tick-skipped` reason, for the same reason `heartbeat`/`gate_enabled` do (#429): a cadence
  that decays silently is a cadence nobody can debug.
- **Why not a first-class 'parked' group state (#864's optional third part).** The issue also
  proposed suppressing the fallback outright for a group whose board tasks are all
  human-testing/blocked/done with zero live delegates. Half of that landed — **zero live delegates**
  is a backoff suppressor, so a group with agents in flight always stays on its base cadence (the
  heartbeat wake is how an orchestrator notices a delegate that went quiet without reporting). The
  board-state half is deliberately deferred: (a) full suppression removes the backstop, and the
  failure it exists for — a host-side poll that fails silently, expired `gh` auth being the ordinary
  case — becomes indistinguishable from genuine silence, permanently; (b) the backoff already
  converges on parked behaviour without any board introspection, since a parked group is delta-free
  by definition and saturates at the ceiling on its own, leaving one wake per day as the entire
  difference; (c) board state is only as true as the orchestrator's board hygiene, which is a weaker
  signal than observed quiet. Whether that half is wanted at all — and if so in what weakened form,
  e.g. a much longer parked cadence rather than zero, which would keep the backstop — is tracked in
  issue #873.
- **Degrade, don't deny.** A `gh` failure on either call (auth, not installed, transient) simply
  skips that half of the diff for the current scan; the poll attempt is still stamped (so a
  persistently-failing `gh` isn't retried every 60s), and the bounded fallback covers the group
  regardless — a poller outage can make the gate less useful, never silence the orchestrator past
  the fallback's own interval.
- **#429 benchtest finding: the gate computed but the audit couldn't tell you so — and it could
  never actually engage in the first place.** A live testbed session
  (`loomux-testbed-cc077f09`) logged six idle ticks over ~45 minutes, every one with
  `intake_summary: null`, and every one still delivered — the generic "run your monitoring
  cadence" prompt, with zero token economy over pre-#332 behavior. First pass added three
  observability fields to the `idle-tick`/`idle-tick-skipped` audit entries — `gate_enabled`,
  `suppressed`, `heartbeat` — so a skip, a real-signal fire, and a fallback-only heartbeat fire
  are no longer indistinguishable after the fact. But re-examining the exact reported scenario
  (a fresh testbed group, autonomous toggled on mid-session, no special config) surfaced the
  REAL root cause underneath: `intake_poll_minutes` defaulted to `0` (off) with **no setter
  anywhere in the product** to turn it on — not a command, not a UI toggle, not an
  autonomous-mode side effect. Every real autonomous group ran the `gate_enabled: false` legacy
  bypass unconditionally; the observability fix alone would have reproduced the identical
  six-for-six null-tick behavior on a re-soak, just with a `gate_enabled:false` audit line
  explaining why. The gate's own fire/skip decision, once actually engaged, was correct the whole
  time (verified by the pre-existing `intake_gate_*` suite).
  User-directed fix (option 1, over adding a setter/UI): `intake_poll_minutes` is now
  smart-defaulted ON the moment a group is autonomous (see the field's own doc bullet above) —
  the dead-default trap (autonomous ON, gate silently off) is now structurally impossible without
  an explicit `Some(0)` opt-out written by hand into `group.json`. `gate_enabled: false` in the
  audit now means exactly one of two things: a supervised group (the gate is autonomous-only by
  construction), or a deliberate opt-out — never "nobody ever turned this on."

**Coexistence with compact-nudge (below):** the two mechanisms are fully decoupled — compact-nudge
runs as its own background thread (`start_compact_nudge`/`run_compact_nudge`/`compact_nudge_tick`),
never inside `idle_tick_tick`, with its own anti-nag latch (`compact_nudge_notified`) and its own
output-total baseline (`compact_nudge_last_output_total`, kept deliberately separate from this
gate's `last_output_total` — see that field's own doc for the rev-24 finding this avoids: two
readers of one counter starving each other). The only state they share is the read of
`AgentEntry.last_progress_ms`, the quiet clock `idle_tick_should_fire` gates on for both — and this
gate never writes to it differently depending on whether it fires or skips (a skip is purely "don't
delivered the notice"), so compact-nudge's own independent read of that clock is unaffected by
whatever this gate decides for the same tick.

### One `gh` poller (#406)

Both features above shell out to `gh`, and both spend the **same account's** GitHub API budget.
They shipped as two threads on two clocks (`start_notify_poller` at 30s, `start_intake_poller` at
60s) with no coordination and no shared accounting of that budget — low-risk while the intake gate
was default-off, a latent coupling once it isn't. #406 folds them into one loop:

- **`start_gh_poller` is the only background loop in this process that calls `gh`.** It wakes on
  `notify::NOTIFY_POLL_INTERVAL` and runs `run_gh_poll_tick` = `poll_watches(now)` +
  `gh_poll_tick(now, &results)` — the impure/decision split the notify backend already had,
  unchanged, so tests still drive the decision half with a synthetic `PollResult` map and nothing
  in `tests/orchestration.rs` shells out to `gh`.
- **One clock per wake.** `now` is sampled once and handed to `poll_watches`, `notify_tick` and
  `poll_intake`; all three previously read `now_ms()` themselves, on two different threads. Two
  halves of one tick can no longer disagree about when "now" was.
- **Neither cadence changed**, with one stated exception. The notify half runs on every wake, so
  the *wake* cadence a watch waits on is still 30s (what a watch waits on in total is that wake
  plus the tick's own work — see the accepted cost below). The intake half runs only when the
  pure `intake_scan_due(now, last_scan_ms)`
  says its coarser 60s scan floor has elapsed — `OrchRegistry.intake_last_scan_ms`, one
  process-wide stamp, distinct from the per-group `gh`-call floor
  (`intake_last_poll_ms`/`intake::due_intake_polls`, untouched). Without that floor, merging the
  loops would have silently doubled the intake scan rate, which is the one behavior change
  "unify the scheduling" must not smuggle in. The exception: the FIRST scan after launch now
  lands on the first 30s wake rather than after the old thread's full 60s sleep (`None => true`),
  because a stamp only exists once a scan has happened. Harmless and arguably better — the
  per-group `gh` floor is untouched — but it is the one instant where "neither cadence changed"
  is not literally exact, so it is written down rather than rounded off.
- **A stamp in the future rescans.** `intake_scan_due`'s middle arm catches a backwards system
  clock jump (NTP correction, VM resume, manual set), which would otherwise stall the intake
  scan for the whole size of the jump — a `sleep`-driven thread cannot be stalled that way, so
  moving to a stamped clock had to not introduce it. Rescanning re-stamps, so one jump costs one
  early scan; the per-group `gh` floor still bounds what that scan can spend.
- **The pure layers stay separate.** `notify::due_watches` and `intake::due_intake_polls` are
  untouched and separately tested; only the thread and its clock are shared. There is
  deliberately **no** new rate-limit counter: what made the coupling real was two uncoordinated
  clocks, and a budget number nothing reads would be a claim with no consumer (the repo's "a
  claim is a deliverable" lesson). Per-tick `gh` budgeting IS wanted — it is the rest of #406's
  ask — but as a bound a scheduler enforces, not a number it reports; one loop is the
  precondition for it, and "Bounding one tick" below is that bound.
- **What the tests pin.** `app_setup_starts_exactly_one_gh_polling_loop` parses `src/lib.rs`'s
  setup block (the `tests/acl_manifest.rs` precedent) — "how many threads call `gh`" is a property
  of that call list and of nothing else, so a future `start_*_poller` added beside it fails this
  test and nothing else in the suite would notice. The other two pin that one wake services both
  features and that the intake scan keeps its own floor while the notify half runs every wake.

`start_workflow_gate_reload` is deliberately **not** folded in: it reads a file, not `gh`, so it
spends none of the budget this section is about.

### Bounding one tick (#656)

One loop means one tick's `gh` work is the whole process's `gh` work, so that work has to be
finite in both directions: no single call may wait forever, and no single wake may make an
unbounded number of calls. Neither held before #656 — and the first mattered more after #406
than before it, because a hang that used to freeze one feature now stops every `notify_when`
notice in the process, which is what the fleet's "register the watch and end the turn"
discipline (#590) rests on.

- **Per call: `capture_with_timeout`.** `gh_capture` builds the `Command` and hands it to
  `OrchRegistry::capture_with_timeout(cmd, GH_CAPTURE_TIMEOUT)`, which spawns, waits to the
  deadline, and kills the child rather than waiting past it. A timeout is returned as the
  ordinary `Err` both callers already classify: `poll_watches` counts it toward
  `NOTIFY_FAIL_STREAK_LIMIT`, `poll_intake` skips that half of its diff and still stamps the
  attempt. No new failure channel, no new dependency (a std spawn plus a manual deadline —
  every timeout crate in reach pulls `getrandom`-adjacent weight this crate refuses, and none
  of them is needed for one deadline).
- **Why the bound is generous rather than tight.** A false timeout is not free: three of them
  in a row cancel a watch that was about to resolve. A live `gh` call lands in ~1s and a
  genuinely stalled one hangs for minutes or forever, so the two cases separate at any value in
  a wide band, and erring long costs only a slower tick. Worth stating the price plainly: with
  both caps, a tick makes at most 8×2 + 4×2 = 24 calls, so an all-stalled tick is bounded at
  ~8 minutes rather than at infinity. That is a bound, not a comfortable one — it is the
  pathological case (every call stalling at once), and tightening it further is a job for a
  cross-half per-tick budget, which #406 deliberately did not ship and this does not either.
- **Why both pipes are drained on their own threads.** The obvious shape — poll `try_wait`,
  read the output once the child reports exit — deadlocks against the very calls this poller
  makes: `gh issue list --json` on a busy repo outruns the OS pipe buffer, and a child blocked
  writing into a full pipe never exits. That shape would turn a healthy call into a timeout
  every tick, i.e. trade the hang for an outage. On the *timeout* path the readers are left to
  end with their pipe instead of being joined: a grandchild can hold the handle past the kill,
  and joining would reinstate exactly the unbounded wait being removed.
- **What bounds the abandoned readers, per process rather than per tick.** Not joining them
  bounds one call; it does not bound the process, and the case that justifies not joining is
  precisely the case where they never end — while a persistently stalling condition is
  re-polled every tick. So an abandoned reader is parked in `GH_CAPTURE_LEAKED_READERS`, every
  capture first sweeps the ones that have since ended, and a capture is refused once
  `GH_CAPTURE_MAX_LEAKED_READERS` (16 — two per timed-out call, against a 24-call tick) are
  still blocked. Be explicit about the trade: past the ceiling `gh` polling in this process
  stops until the backlog drains, and against a grandchild that holds a pipe forever it never
  drains. That is the deliberate direction — a named `Err` both callers already surface (a
  watch cancelled with a reason someone can read) beats thread growth nothing reports, and it
  is diagnosable where a thousand parked threads are not. In the ordinary stall the child IS
  `gh`: killing it closes both pipes, the readers end at once, and the backlog stays empty.
- **The accounting is an invariant of the whole function, not a step on the timeout path**
  (#699). Once the two readers exist, every exit accounts for both exactly once: the arm where
  the child exited on its own joins them, and *every* arm that gives up on a live child — the
  timeout, and the one where the bounded wait itself errors — goes through one shared
  `abandon_child_and_readers` (kill, bounded reap, park both). The two returns that precede
  the readers, the backlog refusal and a failed `spawn`, have nothing to account for. That is
  an invariant of every *return*; one **unwind** escapes it, deliberately. `std::thread::spawn`
  panics if the OS refuses a thread, so a panic between the two reader spawns would unwind with
  the first handle untracked — the exact residue this rule exists to eliminate, reached without
  passing through any of the arms above. It stays out of scope because the fix would be a
  `Builder::new().spawn()` failure channel invented for a case this process cannot reach: the
  ceiling holds the whole backlog at 16 threads, which is not where an OS refuses one. Recorded
  rather than closed, so the next reader knows it was weighed. The
  wait-error arm was originally a bare `?`, which read like the conservative choice and is the
  opposite of one: a dropped handle is invisible to the sweep, so the ceiling's admission
  predicate answers with a count that omits precisely the readers that will never end — and
  dropping a `std::process::Child` does not kill the process, so the child that keeps them
  blocked stays alive too. Uncounted is worse than parked, never better.
- **No arm of the bound is itself unbounded.** Every wait on a child goes through
  `wait_bounded`, including the post-kill reap on the timeout path (`GH_CAPTURE_REAP_TIMEOUT`).
  A bare `wait()` there would be the one place a "bounded" path could still block forever: the
  kill has landed in every ordinary case, but a child in uninterruptible sleep does not die
  until it leaves that state. Outliving the reap leaves an unreaped child — a PID-table entry
  until the process exits — which is the cheaper residue and, unlike the wait, a bounded one.
- **Per tick: both halves are capped.** The notify half already had `MAX_POLLS_PER_TICK` (8).
  The intake half now has `intake::MAX_INTAKE_POLLS_PER_TICK` (4) — expressed as the same
  *`gh`-call* budget rather than the same group count, since a due group costs two calls. Before
  it, N autonomous groups falling due on one scan wake was 2N sequential round-trips inside a
  single tick, and every watch notice waited behind them. What the cap costs in exchange: a
  group's effective cadence becomes `max(intake_poll_minutes, ceil(N/4) scans)` — the scan
  itself is `INTAKE_POLL_SCAN_INTERVAL` (60s), so 20 autonomous groups all due at once are all
  polled within 5 scans rather than in one burst. That is why `docs/autonomous-mode.md` calls
  the per-group setting a floor rather than a schedule.
- **The cap's ordering is load-bearing, not cosmetic.** `due_intake_polls` reads a `HashMap`, so
  a bare truncation would cut the due list at an arbitrary — but, for a given hash seed, stable
  — point and starve whichever groups landed on the wrong side of it. Sorting by `last_poll_ms`
  (ties broken on the group id, so the answer is stated rather than hash-dependent) makes the
  group this scan deferred the oldest, and therefore the first one the next scan takes: the cap
  is a queue, not a drop. Note the asymmetry this creates with the idiom it copies:
  `notify::due_watches` sorts by `last_poll_ms` with **no** tiebreak, and every watch registers
  at `last_poll_ms: 0`, so a fresh cohort larger than `MAX_POLLS_PER_TICK` is selected among in
  `HashMap` order. That is not a starvation (each polled watch gets a real stamp and leaves the
  zero cohort, so the cohort drains), which is why it is left alone — but if the two selectors
  are ever unified, align `notify` **up** to the tiebreak rather than aligning `intake` down.
- **What the tests pin.** `capture_with_timeout_stops_waiting_on_a_stalled_child` asserts
  ELAPSED time against a child that outlives its bound 20x — an implementation that said "timed
  out" only once `output()` finally returned would satisfy the message and none of the point.
  `capture_with_timeout_drains_output_larger_than_a_pipe_buffer` is the regression pin on the
  reader threads (a ~512 KiB child, past every platform's default buffer). `wait_bounded` is
  pinned on both arms (a live child reported at the deadline, an exited one reported with its
  status), and the reader ceiling on all three of its own: the pure admission boundary, the
  refusal-then-drain cycle (driven through a seam that parks controllable blocked readers, so
  it is deterministic on every platform), and — the one that keeps the rest honest — that a
  timeout whose child left a grandchild holding the pipes really does park its readers where
  the ceiling can see them. Those tests take a shared serial lock, since the backlog is
  process-wide state and this binary runs in parallel. The intake cap is
  pinned pure in `intake.rs` (cap, oldest-first ordering, deterministic tiebreak, and
  two-scans-cover-every-group) and on a live registry in
  `poll_intake_caps_one_scan_and_polls_the_deferred_groups_on_the_next`. None of them shells out
  to `gh`: the subprocess tests drive a deliberately slow/chatty/failing shell command, and the
  registry test's groups point at nonexistent repo paths, so every `gh_capture` inside it
  returns at its "no such directory" guard before a program is resolved.

## Compact-nudge (#287)

The orchestrator pane lives for the whole session and every turn re-reads its entire
history, so its lifetime cache-read volume dwarfs every worker's — observed live during
the #271/#244 arc, where a manual `/compact` at a lull reclaimed the base cleanly and the
templates' existing post-compact re-sync convention (`list_tasks` + `get_state` +
`list_agents`) picked the conversation back up without loss. Loomux already knows when a
pane is genuinely idle; this automates picking the moment instead of waiting for the
human to type `/compact` by hand or the CLI's own emergency auto-compact at the context
limit.

- **Scope.** #287 shipped the loomux-timed heuristic nudge on its own — the original issue
  body's proposal and guardrails, no new agent capability. #328 (filed as a follow-up so the
  comment-driven refinements weren't silently dropped) then pulled the whole discussion back
  into this same PR per a standing directive: mid-flight refinement requests fold into the
  active PR by default rather than deferring. What follows describes the result as ONE
  system, not two bolted together — the heuristic timer is now the **fallback** path, and
  agent-initiated `request_compact()` is the **primary** one, with the offload checklist,
  context-escalation, and mandatory re-injection layered on top of the exact same
  quiet-clock/delivery/latch machinery #287 already established.
- **Reuses the SAME idleness signal as the watchdog and idle-tick, not a second one.**
  `compact_nudge_tick` folds pty output growth into `AgentEntry.last_progress_ms` using
  the identical debounce `idle_tick_tick` uses for the orchestrator
  (`idle_output_is_activity` against the group's existing `idle_activity_floor_bytes`
  guardrail — a real turn resets the quiet clock, a sub-floor statusline repaint does
  not). It does not invent a text-pattern "is this pane at its input prompt" detector;
  "idle at the input prompt" is read the same way the rest of the orchestration backend
  already reads it — sustained output silence, not a busy CLI mid-render. The fire
  decision itself reuses `idle_tick_should_fire` verbatim (the threshold/latch/per-hour-
  cap shape `watchdog_tick` established and `idle_tick_tick` reused first) rather than a
  hand-rolled copy, so a new guardrail concept gets the SAME gate, not a similar-looking
  one.
  **Two readers of one counter need two baselines (rev-24 review finding).** Watchdog and
  idle-tick never watch the same agent (watchdog explicitly skips the orchestrator;
  idle-tick only ever touches an autonomous group's orchestrator), so those two sharing
  `AgentEntry.last_output_total` as their rebaseline counter is safe — there is only ever
  one reader of it at a time. Idle-tick and compact-nudge are different: in the
  autonomous-plus-compact-nudge configuration the feature exists for, they CAN both be
  watching the same orchestrator. An earlier revision had `compact_nudge_tick` rebaseline
  the SAME `last_output_total` idle-tick uses, on every observation regardless of whether
  growth was meaningful. Whichever background loop's 60s tick happened to poll the pty
  first each cycle consumed the growth (rebaselined the counter to the current value);
  the other tick's `idle_output_is_activity` check then always saw a zero delta against
  an already-caught-up baseline, so it could never observe fresh growth again — its own
  anti-nag latch, once set, never cleared. Depending on which background loop happened to
  win the race consistently, that meant compact-nudge firing at most once per pane
  lifetime, or idle-tick silently starving, in exactly the combined configuration the
  feature is for. The fix: `AgentEntry.compact_nudge_last_output_total` is compact-nudge's
  OWN baseline, entirely separate from idle-tick's `last_output_total` — the standard shape
  for two independent consumers polling one monotonic counter (each keeps its own
  last-seen offset, like independent Kafka consumer groups over one log), not a new
  idleness signal: both ticks still derive "was there real growth" from the exact same
  pty `output_total` counter via the exact same `idle_output_is_activity` rule and the
  exact same `idle_activity_floor_bytes` guardrail. `last_progress_ms` — the actual quiet-
  clock timestamp both ticks' fire decisions read — stays a single shared field and is
  safe for both to write: each only advances it after independently confirming real growth
  from its OWN baseline, so a write from either tick can only move the timestamp closer to
  the true last-activity time, never invalidate the other tick's next comparison (which
  reads a different field entirely).
- **Delivery is a plain `deliver_prompt` call, nothing bespoke.** `compact_nudge_tick`
  pastes `/compact` + CR to an eligible pane through the exact same delivery path every
  other prompt uses (`Delivery::MidSession` — no PTY resize, per the hard constraint),
  followed by the optional `[loomux] context compacted — re-sync before acting` notice.
  This means the existing human-input paste guard (#111/#171/#246) governs it for free: if
  the pane's input box holds an unsubmitted human line, `deliver_prompt` holds up to its
  shipped cap and then aborts without pasting — a held compact is simply **skipped, not
  queued**. Nothing in `compact_nudge_tick` retries it; the one-shot latch just leaves it
  latched until the pane produces real output on its own, and the next natural quiet window
  gets its own fresh chance. The per-pty delivery mutex `deliver_prompt` takes serializes
  the `/compact` paste and the follow-up notice, so the notice can't land ahead of the
  compact submission.
- **Config: a `Guardrails` field, not a marker file or `.loomux/workflow.yml`.** Two knobs
  were on the table. A marker file (mirroring `notify`/`pause`/`autonomous`) is the
  established shape for a bare on/off toggle, but compact-nudge needs an interval too, and
  autonomous mode's own precedent for "toggle + interval" is two mechanisms working
  together (the `autonomous` marker plus the separate `idle_tick_minutes` guardrail) —
  overkill for a feature with no other behavior the toggle needs to gate.
  `.loomux/workflow.yml` was ruled out entirely: it has no scalar-guardrail schema (only
  blocks/edges/gates), it is repo-authored content that only takes effect when a group opts
  into `advanced_orchestrator`, and it is validated by a much heavier parser
  (`parse_workflow`) built for a different kind of config. The closest precedent is a
  single numeric `Guardrails` field where `0` means off — exactly the shape
  `watchdog_stall_minutes` and `idle_kill_minutes` already use, persisted straight in
  `group.json`, no separate marker. `compact_nudge_minutes` follows that: `0` (the shipped
  default) disables the feature outright; unlike `idle_tick_minutes`, `0` is never floated
  up to a default, since there is no other marker doing the on/off job.
  `compact_nudge_roles` (role names, default `["orchestrator"]`) rides the same
  `Guardrails`/`group.json` path and is live-settable the same way `idle_tick_minutes` is
  (`set_compact_nudge_minutes` / `set_compact_nudge_roles`, `orch_set_compact_nudge_minutes`
  / `orch_set_compact_nudge_roles`, mirroring `orch_set_idle_tick_minutes`).
- **Per-CLI gate.** `/compact` is a Claude Code built-in with no equivalent on the other
  supported CLIs, so `compact_nudge_cli_supported` gates the nudge to `Guardrails::cli_for`
  resolving to `"claude"` for the eligible agent's role — an unsupported CLI is silently
  excluded rather than typing a slash command it won't understand.
- **`request_compact` (#328): agent-initiated, self-scoped, no new trust surface beyond a
  one-bit flag.** An MCP tool (shared tier — every non-solo role, not orchestrator-only) that
  sets `AgentEntry.compact_requested` on the CALLING agent's own entry, resolved from its MCP
  token exactly the way `report`/`message_orchestrator` self-scope — no `group_id`-as-path-
  segment, no cross-pane power, the same discipline every other orchestration command is held
  to. It does NOT write `/compact` immediately: the agent calls it mid-turn (as its LAST
  action), so an immediate pty write would land as a queued message into an active turn.
  Firing waits for `compact_nudge_tick`'s next observation of the pane genuinely quiet
  (`compact_request_should_fire` — no minutes-threshold wait, since the request itself is the
  trigger, but still gated by the shared per-hour cap and the per-CLI check). Because the
  request is self-initiated, it deliberately bypasses `compact_nudge_roles` — a worker can
  request its own compact even though it isn't in the group's heuristic-eligible role set;
  role-gating is a policy about which panes loomux nudges *unprompted*, not about who may ask
  for themselves. An unsupported CLI returns a clear error and sets nothing, rather than
  flagging a request that can never fire.
- **Pre-compact offload checklist: a soft warning, never a block.** `request_compact`'s
  response string carries `compact_checklist_warning` when the calling orchestrator's
  `AgentEntry.last_state_write_ms` (stamped by the `set_state` MCP handler — self-scoped sign
  of life, same pattern as everything else here) is stale past
  `SET_STATE_RECENCY_WINDOW_MS`. The tool call always succeeds regardless — this is advisory
  text riding the return value, not a gate, matching the issue's explicit "warn, never block."
  Meaningless for non-orchestrator callers (`set_state` isn't even available to them), so it's
  silently omitted for those.
- **Context-usage escalation: an exact transcript-recorded figure, not a byte proxy.** The
  issue offered two options — Claude Code's own status-line/JSON hook if a clean signal
  exists, else approximate from pane bytes. Neither was quite right: loomux doesn't invoke
  Claude Code's status-line hook at all today (the existing `parse_session_cost` only scrapes
  *rendered pane text*, and only as cost tracking's own last-resort fallback — see
  `doc/design/group-cost-tracking.md`), and a byte-count proxy would be a second, cruder
  guess sitting next to a feature that already reads the CLI's own transcript for tokens.
  `usage::latest_context_tokens` reads the SAME transcript `group_usage` already reads
  (`~/.claude/projects/<cwd>/<session>.jsonl`), but asks a different question: not the
  cumulative sum across the whole session (that's `parse_claude_transcript`, for
  billing), but the LATEST assistant message's `input_tokens + cache_creation_input_tokens +
  cache_read_input_tokens` — the size of what was actually sent as context for the most
  recent turn. Self-correcting after a compact (the next turn's figure drops right back
  down), and exact where it applies — the honest gap is the ASSUMED context window
  (`CLAUDE_CONTEXT_WINDOW_TOKENS = 200_000`, since loomux has no signal for which tier a
  session is on); erring toward the smaller window is the safe direction, since it can only
  make the escalation arrive a little early, never late. `compact_context_threshold_percent`
  (0 = off, the default) gates it entirely.
  Crossing the threshold fires `compact_escalation_notice` ONCE (an anti-nag latch, cleared
  once usage drops back under threshold — e.g. after a compact lands) and gives the agent
  that same tick to self-request. Only on a LATER tick, still over threshold and still not
  self-requested, does loomux set `compact_requested` on the agent's behalf — deliberately
  split across two ticks rather than done together, so the fallback request can never race
  the notice that's supposed to warn about it first (same-tick would mean `/compact` could
  already be firing by the time the notice's own delivery goes out).
- **Mandatory post-compact re-injection, detected once, reused by all three trigger paths.**
  The hardest of the issue's four "needs real design work" callouts was reliably detecting
  "compaction just finished" from pane output. The answer turned out to already exist:
  `compact_nudge_tick`'s own busy/quiet detector (the same `idle_output_is_activity` check
  that drives the quiet clock) IS a compaction-completion detector once a pane is marked
  `compact_pending` — busy (real output growth while pending, `compact_seen_busy`) then quiet
  again resolves it, with no parsing of Claude's own completion text required. All three
  trigger paths converge on the identical `compact_pending` flag: a loomux-initiated fire
  (heuristic or requested) sets it directly; a human typing `/compact` manually is detected
  via `human_typed_compact_detected` scanning the pane's own ANSI-stripped output tail for a
  standalone `/compact` token (the terminal echoes typed input like any other line) — gated
  by `MANUAL_COMPACT_DETECT_WINDOW_MS` against the pane's `last_user_input_ms` so the tail's
  bounded ring buffer can't replay an ALREADY-handled compact and re-trigger detection.
  Resolution delivers `compact_reinjection_notice`, which — AT THE TIME #328/#329 were built —
  always embedded the pane's ACTUAL kickoff instructions file, read back verbatim from the
  durable file `write_instruction_files` already writes at spawn, not a pointer telling the
  agent to go re-read it (the issue's explicit preference: no reliance on the agent locating a
  file). **This full-embedding default no longer holds — see "#417 correction round 5: slimming
  the re-grounding notice" below**, which changed the FACTS this decision rested on (#416 gave
  the contract a durable home OUTSIDE this notice entirely, for almost every agent) rather than
  reversing the reasoning itself: "no reliance on the agent locating a file" is exactly as true
  as ever for the one case that still gets the full embedding. This supersedes #287's optional
  immediate post-paste notice entirely — sending both would be redundant, and the immediate
  version risked landing while compaction was still running; the mandatory version only ever
  fires once compaction is actually observed to be done.
- **Template.** The orchestrator persona's existing "Compact at lulls" invariant (predating
  even #287 — it used to tell the orchestrator to type `/compact` itself) is rewritten to
  call `request_compact()` as the primary mechanism, name the offload checklist as a
  precondition, and drop the old "treat the next turn like a session start" instruction now
  that loomux's own mandatory re-injection does that automatically.
- **Scope trim, stated rather than silently dropped.** The issue floats a config knob
  choosing between "pointer" and "full re-injection," defaulting to full for orchestrators.
  Only full re-injection is built — it is both the stated default AND the recommended,
  more-robust option (no reliance on the agent finding a file), so a second mode whose whole
  purpose is being the less-recommended alternative wasn't worth the added config surface
  here. **Revisit if a real need for the pointer mode shows up** — #416 turned out to BE that
  need: not a config knob a human toggles, but a per-agent FACT (`AgentEntry::contract_on_
  system_layer`) about whether the contract has a durable home outside this notice at all. See
  "#417 correction round 5" below — still no config surface, since the choice is now something
  loomux already knows at spawn time, not a preference to expose.

### #329 expansion: the directive ledger and the fourth trigger path

#328's re-injection fixes *role* identity: an agent that comes back from a compact is
re-grounded in the contract it was kickoff'd with. It does nothing for *session-scoped*
state — a live-only fact the human handed the agent mid-conversation (a scope decision, a
directive, a piece of feedback) that never made it to the board or `set_state`. The
incident that drove this: on v0.10.0 an orchestrator hit the CLI's own emergency
auto-compact mid-task and came back a generic agent with every mid-session human directive
gone — #328's three trigger paths (agent-requested, threshold-escalation fallback,
human-typed `/compact`) all assume something ASKED for the compact and can offload first;
the CLI deciding on its own, unprompted, is exactly the case none of them cover. This
expansion adds a fourth trigger path for that case, and a durable diary so a directive
survives it even when nothing warned anyone first.

- **Directive ledger: a diary kept at receipt time, not a deathbed dump.** `note_directive
  (text, replace?)` is a new MCP tool, in the identical shared tier as `request_compact` —
  every non-solo role (orchestrator, worker, reviewer, planner), self-scoped to the CALLING
  agent's own entry with no `group_id`-style path segment and no cross-pane power, the same
  discipline held everywhere else in this file. The whole point is timing: an emergency
  auto-compact gives no warning turn, so "offload what matters before it lands" (#328's
  advice for the other three paths) doesn't work here — the agent has to have already
  written down the directive the moment it received it, before doing anything else with it.
  A plain call appends one timestamped line to a per-agent ledger file
  (`<group-dir>/ledger-<agent-id>.log`, alongside `audit.jsonl` — human-inspectable the same
  way, per the #240 precedent); `replace: true` rewrites the file wholesale, which is how an
  agent CURATES it — typically right after a re-injection has just shown it its own tail,
  dropping entries that are done or no longer relevant so the diary doesn't grow forever.
  The append path (`append_ledger_line`) reuses `append_audit`'s one-buffer/one-`write_all`
  atomicity rule but needs none of its `AUDIT_LOCK` rotation machinery: a ledger file has
  exactly one writer (its own owning agent) and no rotation, so there is no rotate-vs-append
  race to guard against in the first place.
  **Sanitized in append mode, capped on every write (review N1/N2).** Append-mode `text`
  runs through `notify::sanitize_gh_text` — the exact function `channel_send` already puts
  untrusted text through — before it's written: strips control characters (an embedded `\n`
  would otherwise split one call into several physical lines, breaking the one-line-per-entry
  model `directive_ledger_embed` and the file format both assume) and neutralizes `[`/`]` (so
  a line can never start with a forged `[loomux]` marker once re-embedded verbatim into the
  re-grounding notice). Judged low severity — the ledger is self-authored and self-scoped, so
  an agent can only ever spoof itself, unlike `channel_send`'s cross-pane trust boundary —
  but free to close the same way. `replace` writes its `text` verbatim, unsanitized by
  design: it's the curation path, expected to carry the agent's own prior (already-sanitized)
  entries copied back in, not fresh untrusted input. Separately, the STORED file is capped at
  `DIRECTIVE_LEDGER_MAX_BYTES` (64KB) via `ledger_capped` after every write, append or
  replace: over cap, oldest entries drop first (line boundary), never silently — a non-zero
  drop is audited (`ledger-trimmed`) and named in `note_directive`'s response string.
  Curation via `replace: true` stays the primary, deliberate mechanism; this is only the
  backstop for a session that never uses it.
- **Embedded in the SAME re-injection notice, size-capped with a stated truncation.**
  `compact_reinjection_notice` gained a second parameter — the ledger section, produced by
  `directive_ledger_embed(ledger, cap_bytes, ledger_path)` — folded in after the
  instructions and before the re-sync line. An empty or missing ledger embeds nothing (no
  header with nothing under it) for every agent that has never called `note_directive`, so
  this is a no-op change for a session that doesn't use the feature. `directive_ledger_embed`
  keeps the TAIL when the ledger exceeds `DIRECTIVE_LEDGER_EMBED_CAP_BYTES` (2KB), cut on a
  line boundary so a truncation never slices one entry in half, and always keeps at least
  the single newest entry even if it alone exceeds the cap — a directive is never silently
  dropped for being long, only ever declared truncated, with the count and the full file's
  path named in the embed text (the repo's no-silent-caps rule for bounded-coverage
  features). This is a diary for what the human said, not a replacement for the board or
  `set_state` — durable decisions with lasting consequence still belong there too, and the
  templates say so.
- **The fourth trigger path: detecting the CLI's OWN auto-compact.** #328's three paths all
  converge on the same `AgentEntry.compact_pending` flag, resolved by the shared
  busy-then-quiet detector once compaction is observed to have finished. None of them ever
  SET that flag for an auto-compact the CLI decides on its own — there is no
  `request_compact` call, no heuristic timer fire, and no human typing `/compact` to detect.
  Claude Code renders a spinner line while it auto-compacts, observed (1.0.x) as `✢
  Compacting conversation… (esc to interrupt · 8s · ↓ 172 tokens)` — a stable `Compacting
  conversation` core wrapped in a spinner glyph and a live elapsed-time/token-count suffix
  that both change every repaint. `auto_compact_banner_detected(cli, tail)` matches only
  that stable substring, via a per-CLI substring table
  (`auto_compact_banner_substrings`, `SUPPORTED_CLIS`-shaped: keyed by CLI, empty for any
  CLI with no known banner) rather than an `if cli == "claude"` inline in the generic
  pipeline — this repo never bakes one CLI's quirks into product code, and a second CLI's
  banner (should one ever need detecting) is a one-line table addition, not a pipeline
  change. The exact string is a documented assumption, not a guarantee this repo controls:
  if the detector stops firing, re-verifying it against a current Claude Code build is the
  first thing to check.

  **Two rounds of false-positive fixes, both worth recording — the second was found in
  review, not by the author.**

  *Round 1 (recency, fixed before review): `!currently_quiet`, not a duration window.* The
  first draft gated detection on a `MANUAL_COMPACT_DETECT_WINDOW_MS`-style duration compared
  against `AgentEntry.last_progress_ms`, mirroring `human_typed_compact_detected`'s guard
  against a stale tail. That mirror doesn't hold: `last_progress_ms` is *rewritten to `now`*
  by this same tick's own growth check whenever the pane is busy for ANY reason, so a window
  compared against it reads as "fresh" on every busy tick regardless of why the tail
  contains the banner text — it would have passed even for banner text left over from a
  compact that resolved an hour ago, on the next unrelated busy tick. Fixed to gate on
  `!currently_quiet` instead — this tick's OWN observed growth, not a timestamp.

  *Round 2 (position, fixed in review — B1): a growth gate alone does not close the mention
  case, because the mention IS the growth.* `!currently_quiet` closes the STALE-banner
  re-trigger, but a reviewer caught the sharper failure it does nothing for: a busy pane
  that PRINTS or DISCUSSES the banner text (a `gh pr diff` hunk, a grep result, a rust
  string literal in a code listing, the model streaming a sentence about this very feature
  — this repo's own source contains the string) satisfies `!currently_quiet` by the mention
  being rendered. The growth and the banner text are the *same event* in that case — the
  strongest possible false correlation, not a rare coincidence — and no recency/growth check
  can ever tell it apart from the real thing. What can: **position**. The real spinner
  renders as the live status line, continuously redrawn in place, with nothing after it
  until compaction finishes. A quoted mention sits in scrolled content with other lines
  following it almost always (the diff continues, the file continues, the sentence has more
  sentences after it). So `auto_compact_banner_detected` now checks only the tail's LAST
  non-blank line, never the full tail — a mention buried in scrollback can no longer surface
  regardless of how much growth accompanies it.

  This is a real reduction in surface, not a perfect one, and the doc says so rather than
  overclaiming: the accepted residual risk is a mention that happens to BE the exact last
  line of output at the instant a tick reads the pane (a streamed reply that ends its turn
  naming the string, with nothing rendered yet after it). Closing that fully would need
  either a structural signal from the CLI itself (see the #397 note below) or a second-tick
  confirmation before latching — judged not worth the added state for how narrow the
  remaining window is; if it stops holding in practice, that confirmation tick is the next
  move, not a broader substring match. `compact_seen_busy` is still set `true` at the moment
  of detection (not left for a later tick), since the banner match itself IS the busy
  signal, caught mid-compaction rather than inferred after the fact.
- **What this does not build.** #397 (filed as a follow-up, not built here) proposes a
  claude-CLI-tier `PreCompact` hook that shapes Claude Code's OWN compaction summary so
  directives survive *inside* it verbatim — a structurally different, more elegant fix than
  a separate ledger file, but Claude-specific and dependent on a hook this repo doesn't use
  anywhere else today. This expansion's ledger-plus-banner-detection approach works
  independently of whether that hook ever lands (and independently of which CLI is
  running), so the two are complementary rather than one blocking the other; #397 stays
  scoped to its own issue rather than folded in here.

### Production incident: the reinjection loop, and why busy-then-quiet was never enough

Live production evidence (a user demo, testbed group `loomux-testbed-cc077f09`) showed the
mandatory reinjection firing on a REPEATING LOOP — the exact opposite of what compact-nudge
exists to prevent, since every cycle only grows context instead of compacting it.

**Reconstruction from `audit.jsonl` + `breadcrumbs.log`.** The human asked the orchestrator to
test self-compaction (call `request_compact`, prove a directive survives via the ledger). Audit
shows `request_compact` called, then — 4m33s later, with **no `compact-nudge` audit entry at
all** (the heuristic/requested path never got a chance to paste `/compact`) — a `compact-
reinjection` fires, followed by two more at ~2-minute intervals. `breadcrumbs.log`'s `delivery`
lines confirm each reinjection was typed/confirmed successfully, and confirm `PtyManager::
write_bytes` (what `deliver_prompt` uses) never touches `user_input_ms` — ruling out a
theory that loomux's own paste was refreshing the manual-detection recency gate. The absence of
any `compact-nudge` audit line means some OTHER detector armed `compact_pending` before the
heuristic/requested check ever ran in that tick — and the repeating ~2-minute cadence (one
response cycle per reinjection, with no dependency on the *original* human input's age) points
at `auto_compact_banner_detected`: this is the loomux repo itself (dogfooding), the orchestrator
was explicitly asked to discuss the compact-nudge feature it was testing, and both `mod.rs` and
this very design doc contain the literal string `Compacting conversation` — exactly the mention
the B1 fix's own documented residual risk describes (a mention that happens to land as a tick's
last rendered line).

**Why `!currently_quiet` (the B1 fix) didn't close this.** B1 closed the *stale-tail*
false-positive (a banner sitting in unchanged scrollback). It does nothing for a detector that
re-satisfies on **fresh, repeating** content — an agent that discusses the feature every
response cycle produces NEW growth, NEW last-line matches, every time. Busy-then-quiet was
never actually evidence a compaction ran; it was a proxy that happened to hold as long as the
four trigger paths were themselves reliable. This incident proved at least one of them isn't.

**The fix (D2 + D3): require confirmed evidence, and make resolution unconditionally one-shot.**

- `compaction_confirmed(baseline, current)` — a new pure function — requires the agent's
  context-token reading (`usage::latest_context_tokens`) to have dropped to at most 70% of the
  baseline captured the moment `compact_pending` was set, for ANY of the four trigger paths.
  Context tokens only grow across ordinary turns until a real compaction resets them, so this is
  a strong, cheap-to-check signal — and it **fails closed**: no baseline, no current reading, or
  no real drop all resolve to "not confirmed," never a guess. A missed reinjection is a missed
  convenience; an unconfirmed one delivered anyway is the production incident.
- The busy-then-quiet resolver in `compact_nudge_tick` now calls `compaction_confirmed` before
  delivering reinjection, and **always** clears `compact_pending` / `compact_seen_busy` / the new
  baseline field regardless of the outcome — confirmed or discarded, one resolution attempt per
  arming. This is what makes the state machine structurally loop-proof: a detector that
  re-satisfies every cycle can re-arm as many times as it wants, but each arming resolves to at
  most one discarded, context-growth-free no-op. A repeating false positive becomes a repeating
  silent discard (audited as `compact-pending-discarded` for visibility — this exact gap, no
  record of which detector armed `compact_pending` or that a resolution had been discarded rather
  than genuinely completed, is why this incident took real forensic work to root-cause), never a
  repeating reinjection.
- `agent_context_tokens` — a new impure reader — supplies the raw token count for EVERY Running
  Claude-CLI agent with a resolvable session, not gated on the group's escalation threshold like
  the older `agent_context_percents` (which exists purely for the opportunistic escalation
  notice and can afford to skip the transcript read where nobody asked for it). The confirmation
  signal has to hold for every agent that could ever enter `compact_pending`, not only ones
  additionally opted into escalation — accepted cost: an escalation-enabled group's agents get
  the transcript read twice per tick (once per function) rather than restructuring the older,
  well-tested escalation path to share one read.

**What was considered and NOT built: D1, synchronous paste confirmation.** The review's initial
framing asked whether `deliver_prompt`'s result should gate `compact_pending`, since the
heuristic/requested path sets it *before* attempting the `/compact` paste. Investigating showed
this isn't the fix it first appears to be: `deliver_prompt` is fire-and-forget by design — it
spawns a background thread to type/confirm and returns as soon as that thread is spawned, so its
`Result` reflects whether the pty/app handle existed, never whether the paste actually landed
(the real failure mode named in the review — a hold due to unsubmitted human input, silently
aborted — happens entirely on that background thread, after this function has already returned).
Gating `compact_pending` on that `Result` was tried and reverted: it also broke the test suite's
existing, deliberate decoupling of the *fire decision* from delivery infrastructure (unit tests
exercise `compact_nudge_tick` with no real pty/app handle at all, so `deliver_prompt` always
errors synchronously in that environment — by design, not a gap to close). D2/D3 already cover
the practical risk D1 was aimed at: whatever the reason a real compaction didn't happen — a
silently held paste, a dead agent, or (this incident) a detector that was simply wrong — no
context-token drop means no reinjection, unconditionally.

### rev-42 delta: the D2 gate deadlocked the primary path, and how the fix splits by epistemic state

Review of D2/D3 (above) found a NEW blocking defect the loop fix itself introduced: a uniform
`compaction_confirmed` gate across all four trigger paths deadlocks the loomux-initiated path
(heuristic fallback / `request_compact`) — the primary, happy-path way a compaction starts.

**The deadlock.** `usage::latest_context_tokens` reads the LATEST assistant turn's token count
from the transcript. On the loomux-initiated path, loomux pastes `/compact` itself and then waits
for busy-then-quiet; no further turn occurs before the reinjection this gate exists to authorize
— the reinjection notice IS the next turn. So the confirmation reading is always taken *before*
any turn could show the drop, reads as still-high, and D2's fail-closed design (correctly, by its
own logic) resolves to a discard. Proved empirically, not assumed: a real dogfood transcript
(`usage::tests::real_transcript_proves_the_token_drop_is_a_next_turn_phenomenon_rev42_q1`) shows
`latest_context_tokens` reading 516,593 (stale, pre-compact value) immediately after a real
`compact_boundary` marker, and only dropping to 45,958 once a further turn is appended. A
time-based confirm-wait cannot fix this — no turn is ever coming on this path. Left as D2 shipped
it, this is silent, permanent identity loss on the path most compactions actually take — worse
than the reinjection loop it replaced, which was at least visible (repeating reinjections), not
silent.

**The fix: split confirmation by what loomux actually knows, not one gate for all four paths.**

- **Loomux-initiated arms** (the heuristic fallback and `request_compact` fire, i.e. exactly the
  code that pastes `/compact` itself) set a new `compact_pending_trusted = true`. The resolver
  skips `compaction_confirmed`/`inferred_compaction_confirmed` entirely for these — busy-then-quiet
  IS the signal, same as before D2 ever existed, because loomux has positive knowledge the command
  was submitted. This was never the false-positive path the incident occurred on (the incident's
  own audit trail showed zero `compact-nudge` entries — see above).
- **Inference arms** (manual-`/compact`-typing detection, auto-compact-banner detection) set
  `compact_pending_trusted = false` and keep a hard gate — these are the paths that can still be
  fooled by a mention or an ordinary turn, same failure mode the incident actually hit. The gate is
  now `inferred_compaction_confirmed`, widened beyond the token-drop check alone.
- **The widened signal: `compact_boundary`.** Claude Code writes a `type: "system", subtype:
  "compact_boundary"` transcript line the moment compaction completes, carrying
  `compactMetadata.preTokens`/`postTokens` — unlike the token reading, this is available
  *immediately*, on the completion turn itself, with no next turn required. `usage::
  compact_boundary_count` counts these; `inferred_compaction_confirmed` treats EITHER a confirmed
  token drop OR a rise in this count (baseline vs. current) as sufficient. Verified against the
  same real dogfood transcript (the marker is present, count 1, at the exact point the token
  reading is still stale) rather than a synthetic fixture — a real transcript excerpt was the only
  way to know what Claude Code's own transcript format actually contains here.
- **`AgentEntry` gains two fields** alongside the existing `compact_pending_baseline_tokens`:
  `compact_pending_baseline_marker_count` (the `compact_boundary_count` reading at arm time) and
  `compact_pending_trusted` (which branch the resolver takes). All three — plus the pre-existing
  token baseline — are unconditionally cleared on resolution, preserving D3's one-shot guarantee:
  confirmed or not, trusted or not, one resolution attempt per arming.
- **`agent_context_tokens` + `agent_context_percents`'s double transcript-read (Q4) is closed.**
  A new `agent_context_signals` does ONE bounded tail-read per Running Claude agent via
  `usage::compaction_signal_in`, returning both the token reading and the boundary-marker count;
  `agent_context_percents` now derives its percents from that shared map instead of re-reading the
  same transcripts itself.
- **The discard audit is enriched (Q3).** `compact-pending-discarded` now carries a `reason` plus
  the baseline/current token values, so an audit log alone can distinguish a harmless false-
  positive discard from what would otherwise look identical to a lost real compaction — exactly
  the ambiguity that made the original incident require real forensic work (breadcrumbs + audit
  cross-referencing) rather than a single log line.

Regression coverage: `compact_nudge_tick_never_loops_when_the_false_signal_repeats_every_cycle`
(D4) stays pinned at zero reinjections — the inference-path fix is unchanged by this delta.
`compact_nudge_tick_reinjects_a_loomux_initiated_fire_even_with_no_confirmed_token_drop` is the new
mirror: a loomux-initiated fire with a FLAT token reading (no drop, no next turn, exactly the
deadlock scenario) still reinjects exactly once — verified red-before-green against a neutered
trusted-bypass before this fix landed.

### rev-42 delta, round 2: the re-demo that resolved but never notified

The user re-demoed against the round-1 fix (`b8d9636`, testbed group `loomux-testbed-cc077f09`).
Compaction itself now worked — a genuine `request_compact` → `/compact` paste → real "Compacted"
output, context dropping to 38K/4%. But the mandatory re-grounding notice never arrived; the
codeword only survived via the agent's OWN pre-compact summary, not the ledger reinjection —
the feature under test did not demonstrably fire.

**Forensics first.** `audit.jsonl` + `breadcrumbs.log` around the exact window of the confirmed
`/compact` paste (`compact-nudge` audited, `prompt-typed` confirmed by breadcrumbs) show **no**
`compact-reinjection` audit, **no** `compact-pending-discarded` audit — nothing — for over three
minutes, while the agent was demonstrably back to normal work (a `get_state` tool-call at the tail
of the log). Both terminal audit outcomes are emitted unconditionally by the resolver before any
delivery is attempted, so their total absence means the busy-then-quiet resolver never even
reached its confirm/discard branch — not a delivery that was held, skipped, or lost (the user's
original H1 hypothesis), but a **precondition that was never satisfied** at all.

**Root cause: `compact_seen_busy` depended entirely on output-byte growth clearing
`idle_activity_floor_bytes`.** The loomux-initiated arm starts with `compact_seen_busy = false`
and waits for a LATER tick to observe real terminal-output growth past the floor. This is
fine for a compaction whose own rendering is substantial — but a real, genuine compaction can
render little enough that no single inter-tick delta clears a floor tuned to filter ordinary
repaint noise. Unlike the two INFERENCE arms (banner detection, manual typing), which set
`compact_seen_busy = true` **immediately at arm time**, straight from the very evidence that
armed them, the loomux-initiated arm has no such alternate evidence — it was purely waiting on a
byte-growth observation that, this time, never came. `compact_pending` then stays `true` forever:
stuck, silent, and (as a side effect) blocking every future compaction for that agent too, since
`!a.compact_pending` gates every arm site.

**Fix 1 — widen "seen busy" with the `compact_boundary` marker, for every arm.** A rise in
`usage::compact_boundary_count` since the arming baseline is direct, floor-independent proof a
compaction happened — Claude Code writes it the instant compaction completes, with no dependency
on how much text it also rendered to the terminal. `compact_nudge_tick`'s resolver now treats
`a.compact_seen_busy || marker_rose` as "seen busy" (still gated on `currently_quiet`, so the
"never paste over a live stream" property is untouched — this only widens what counts as the busy
half, never relaxes the quiet requirement). This applies uniformly to all three arms: the
loomux-initiated arm no longer has a real compaction go unnoticed just because it rendered little
text, and the manual-detection inference arm (which shares the same "wait for a later busy tick"
shape) gets the same protection.

**Fix 2 — the one-shot latch now waits for a CONFIRMED delivery, with bounded retry.** Independent
of the round's actual root cause, the user's fix contract required this regardless: `deliver_prompt`
is fire-and-forget (D1's finding, above) — its `Result` says only whether the paste *attempt*
spawned, never whether it landed. The previous design cleared `compact_pending` the instant a
reinjection was *decided*, with no feedback loop if that specific delivery was held, aborted
(`PasteDecision::Abort`, box occupied with unsubmitted human text), or otherwise never confirmed.
Now:
- `AgentEntry.compact_reinject_attempted_ms: Option<u64>` — set the tick a reinjection is decided
  (or retried), cleared only once its delivery confirms or the retry budget is spent.
- `AgentEntry.compact_reinject_attempts: u32` — 1-indexed attempt count, bounded by
  `MAX_REINJECT_ATTEMPTS` (3).
- A new `DeliveryConfirmation` (mirroring the private `DeliveryOutcome` used for stranded-text
  flush) is threaded into `compact_nudge_tick` as an ordinary input map — `agent_last_deliveries`
  is the impure reader (resolves each agent's CURRENT `pty_id` against `self.last_delivery`) that
  supplies it in production, keeping the resolver itself synthetic-input testable (unit tests have
  no live pty/app handle to exercise `deliver_prompt` for real — same reasoning as D1's rejection).
- Each tick, while a reinjection is in flight: if `delivery_confirmations` shows a delivery that
  started at-or-after the attempt AND confirmed, the latch releases (`compact-reinjection-
  confirmed`, audited). If `REINJECT_CONFIRM_TIMEOUT_MS` (5 minutes — comfortably past
  `deliver_prompt`'s own worst-case hold chain) elapses with no confirmation, a bounded retry fires
  (re-audited as `compact-reinjection` with an `attempt` field). After `MAX_REINJECT_ATTEMPTS`,
  a still-unconfirmed reinjection is abandoned — audited (`compact-reinjection-abandoned`), latch
  released anyway: a lost re-grounding is a real gap, but a permanently wedged agent (unable to
  ever arm a future compaction) is worse.

Regression coverage: `compact_nudge_tick_resolves_a_loomux_initiated_fire_via_the_boundary_marker_
when_output_never_clears_the_busy_floor` pins the actual root cause (a flat output map, only the
marker rising — verified red-before-green against a neutered marker check).
`compact_nudge_tick_retries_a_reinjection_whose_delivery_never_confirms_then_delivers_exactly_once`
and `compact_nudge_tick_abandons_a_stuck_reinjection_after_the_retry_budget_and_frees_the_latch`
cover the confirmed-delivery contract (verified red-before-green against the previous
clear-on-decision behavior). All prior reinjection tests were updated to supply a
`DeliveryConfirmation` before asserting final resolution; `compact_nudge_tick_never_loops_when_
the_false_signal_repeats_every_cycle` (D4) stays pinned at zero, unaffected.

### #535: the retry keys on the agent's ACKNOWLEDGMENT, not on our own delivery confirmation

The delivery-confirmation gate above closed a real gap, and then became one of its own. The
signal it waits on — `submit_sent_ms >= attempted_ms && confirmed` — is loomux **watching its own
paste**, and that sampler misses routinely on a busy or repainting pane (~25 false `delivery
unconfirmed` alarms in one observed session; the #451/#496/#522/#528 lineage). Nothing in the loop
ever observed whether the **agent** had re-grounded. So on a pane where the submit went unseen, a
re-grounding that had *landed* was re-pasted up to twice more into a working agent, and could
finish as a `reinjection-abandoned` record claiming a contract restore that had in fact happened.
Reported live: a copilot worker sitting at 2/3 while visibly on-track, correctly ignoring the
duplicate because it already had the contract.

#### The shared agent-activity clock

`AgentEntry.last_mcp_activity_ms`, stamped by `OrchRegistry::note_agent_ack` from **one** site —
the `tools/call` arm of `mcp::dispatch`. It means exactly one thing, and the distinction is the
whole point of the mechanism:

> **the agent's own code path executed** — a live process authenticated with this agent's token
> and invoked a loomux tool. It does **not** mean *the pane painted*.

Three properties, each load-bearing: **role-agnostic** (an orchestrator compacts and gets
re-grounded like anything else); **stamped before the tool runs**, so a *rejected* call counts too
(a permission denial proves the agent is alive and executing just as well as a success); and
**monotone** (`max`, never a bare assignment — a clock a later caller could rewind would let one
consumer erase another's evidence).

None of the three clocks that already existed can answer this. `last_progress_ms` is rewritten to
`now` by pty **output growth** in three separate ticks, so repaint noise above the activity floor
advances it — it answers "did the pane emit", not "did the agent act". `note_agent_activity` (its
writer) is stamped from exactly ONE tool, `message_orchestrator`, and is an explicit no-op for
`Role::Orchestrator`. `DeliveryConfirmation` is the unreliable signal this section exists to stop
relying on.

#### The settling floor (rev-15 finding 2)

`attempted_ms` is when loomux **decided** to paste, not when the agent could first have read
anything: `deliver_prompt` waits for the pane to go quiet before pressing Enter, bounded by
`SUBMIT_MAX_WAIT` (45s), then makes spaced blind retries. So a tool call the agent had already
decided on during its *previous* turn, arriving milliseconds after the decision, is
indistinguishable from a response — and would resolve the phase for a notice the agent provably
had not yet seen. That is a **false landed-signal**, the same family #112 removed in its
output-burst form and #522 in its idle-pane form; re-introducing it here through a different door
would defeat the point of the batch.

So the call site compares against `attempted_ms + REINJECT_ACK_SETTLE_MS`, never bare
`attempted_ms`. The constant is a **const expression over every unconditional stage of
`deliver_prompt`'s submit path** — the echo-verified typing loop (`ECHO_ATTEMPTS` ×
(`ECHO_WINDOW` + `ECHO_RETRY_DELAY`)), `PASTE_SUBMIT_DELAY`, `SUBMIT_MAX_WAIT`,
`SUBMIT_CONFIRM_WINDOW`, and the **sum** of `SUBMIT_RETRY_DELAYS` — so it moves by construction if
any of them does, and every one of those constants carries a back-pointer to it. 63,600ms today.
It costs the signal nothing: ~21% of `REINJECT_CONFIRM_TIMEOUT_MS`, leaving ~236s in which a
genuine ack still resolves the phase, and an ack landing inside the floor is not lost, merely not
counted yet (agents call loomux tools repeatedly; if none ever comes, the unchanged retry path
runs, which is the pre-#535 behaviour).

**This constant was wrong twice before it was right, both times short — the unsafe direction — and
both times in a way that read as rigorous.** Recorded rather than quietly corrected, because the
failure mode is not arithmetic; it is a sum that *looks* complete, which is precisely what stops
the next reader from checking:

- **2.6s short (rev-22).** It read `SUBMIT_RETRY_DELAYS` as absolute offsets and took "the last
  one" as 4.5s. They are sequential `sleep`s, so the last blind Enter lands at their **sum**
  (+7.0s) — and `SUBMIT_CONFIRM_WINDOW`, between the first Enter and that loop, was omitted.
- **11.2s short (rev-28).** The sum covered only the last three stages while the doc claimed the
  whole ordinary path. Two stages that run on *every* delivery — the echo-verify typing loop and
  `PASTE_SUBMIT_DELAY` — sit before them and were missing. Same shape as the first error: a
  complete-sounding enumeration over an incomplete set.

The rule that follows, and it is in the constant's own doc: **a new stage added to
`deliver_prompt` before the last Enter belongs in that expression.**

A third correction, of a claim rather than a number: the doc used to say no activity before the
floor could be a response "as a matter of ordering rather than of judgement". That is false
regardless of the arithmetic. Three `wait_for_question_clear` checkpoints (`QUESTION_HOLD_MAX`,
120s each) and `HUMAN_INPUT_BLOCK_BOUND_MS` (10 min) can push the Enter past the floor. Those are
**deliberately excluded**: they are conditional on an exceptional pane state, and folding them in
would push the floor past `REINJECT_CONFIRM_TIMEOUT_MS` and stop the mechanism resolving anything
at all. **That residual is routed to #546**, with the related question of what could prove the
notice was *read* rather than that the agent is merely executing. The floor is a worst-case bound
over the unconditional path and a judgement about the rest — and now says which is which.

Not the delivery ledger's own `submit_sent_ms`, which would be exact: that would re-couple the
acknowledgment path to the delivery bookkeeping this whole change exists to stop depending on. A
worst-case bound derived from our own timing constants needs no sampler to be working.

The floor lives at the call site, not inside `agent_acted_since`, so the predicate stays a bare
reusable timestamp comparison — but its doc states that **a caller comparing against a paste time
owes the floor**, because a caller that skips it is wrong rather than merely stricter. #539's
detector will be comparing against a paste time too.

Deliberately built as a **shared** mechanism rather than shaped to its first caller:
`agent_acted_since(last_mcp_activity_ms, since_ms)` takes a bare timestamp pair, and
`OrchRegistry::last_mcp_activity_ms` is a public reader. #539 — the unconfirmed-delivery detector,
whose `unconfirmed_disposition` (#522/#528) reads only BOX structure and has no activity evidence
at all — is the intended second consumer. It is **not** wired here; #535 adds no second call site.

#### `reinject_disposition`

Pure, four-way (`Resolved` / `Wait` / `DeferBusy` / `Retry`) so each outcome is separately
auditable and directly pinnable. **The ordering is the contract:**

1. `confirmed_delivery || acked` ⇒ `Resolved`. Evidence of a landing beats the clock — otherwise
   the bug survives. The old signal is kept and checked first: when it fires it is authoritative;
   it was only ever wrong by *omission*.
2. `elapsed < timeout` ⇒ `Wait`. The clock beats busy-ness, so a chatty pane cannot defer before
   it has even waited. (This is also why our own pasted notice echoing back can never cause a
   spurious deferral: the echo lands inside this window.)
3. `busy` ⇒ `DeferBusy`, bounded by `REINJECT_BUSY_DEFER_MAX_MS` **measured from the original
   attempt** — a pane that simply keeps painting cannot extend the deferral a tick at a time.
4. otherwise ⇒ `Retry`, and at `MAX_REINJECT_ATTEMPTS` abandon, both exactly as before.

**Terminal output is deliberately not part of `acked`,** though #535's own write-up lists it.
`output_total` counts our pasted notice echoing back plus statusline/spinner repaint frames
(#480) — the reason `idle_output_is_activity` exists. And folding growth into `acked` would make
`DeferBusy` **structurally unreachable** (busy ⇒ growth ⇒ ack ⇒ `Resolved`): an arm, an audit
action and this paragraph all describing a behaviour the code could never have. Output feeds
`busy` only, where its weakness is harmless — it defers an attempt, it never claims one succeeded.

**The deferral is bounded** because scope's safety net depends on it: unbounded, a pane that never
goes quiet (an animated statusline above the group's `idle_activity_floor_bytes`) would suppress
the retry forever — on exactly the panes that misreport most. A **constant, not a `Guardrails`
knob**, per #518's own call that a knob with no correct second setting only ever gets set wrong.
`busy_defer_max_ms == 0` disables the deferral (pre-#535 behaviour) rather than expiring
instantly, the same convention as `human_input_block(.., bound_ms)`: a mis-set `0` must degrade to
the old behaviour, never to "defer nothing, ever".

#### Observability

The terminal resolution carries `source`: `"delivery"` (our submit sampler saw it) vs
`"activity"` (the agent answered). Two different facts — a timeline conflating them cannot show
whether this fix is working in the field, which is the same provenance distinction `to_hook_armed`
already draws. (#546 later split these onto two *actions* —
`compact-reinjection-confirmed` and `compact-reinjection-liveness-only` — because a shared
action name meant the field was the only thing carrying the difference; see "#546: the claim
and the evidence" below.) New `compact-reinjection-deferred-busy`, because "no retry fired" must read as a
deliberate choice rather than a silently dropped one (`to_hook_native_skip`'s reason). Latched per
attempt via `AgentEntry.compact_reinject_busy_deferred` — the poll runs every 10s while an arm is
open, so an unlatched audit would write ~30 identical lines per deferral.

Regression coverage: `a_landed_re_grounding_is_never_re_sent_once_the_agent_itself_answers` pins
the live defect (and supplies **no** `DeliveryConfirmation` anywhere, which is exactly the
production case that misbehaved); `an_mcp_call_before_the_re_grounding_is_not_an_acknowledgment_
of_it` is its pair, proving the comparison is read rather than mere non-zero-ness;
`a_genuinely_lost_re_grounding_still_retries_and_still_abandons_at_the_bound` pins that the safety
net survives; `an_in_flight_tool_call_from_the_previous_turn_is_not_an_acknowledgment` pins the
settling floor from both sides — 1ms short of it does not ack, exactly at it does, so a floor set
so wide that nothing ever acked could not pass; `a_retry_is_never_spent_into_a_live_turn_but_the_
deferral_is_bounded` covers both halves of the deferral;
`a_permanently_busy_pane_still_reaches_the_abandoned_record_and_the_lost_badge` drives a pane that
is never once quiet all the way through three attempts to the terminal state and asserts
`compact_last_lost_reason` — the field `compactionstatus.ts` renders as the "re-grounding lost"
badge — rather than only the attempt count, so a future edit that reset the clock inside the
`DeferBusy` arm (hanging forever) could not pass;
`a_deferral_yields_to_the_acknowledgment_it_was_waiting_for` pins the
ordering as behaviour rather than as a claim in a comment; and
`every_mcp_tool_call_stamps_the_shared_activity_clock_for_every_role` pins the wiring — the
orchestrator case, the rejected-call case, and monotonicity.

### #410 (round 6): the arm-pending timeout, and a request-starvation fix alongside it

A third re-demo hit a new symptom: `request_compact` answered "a compact is already in flight for
this pane" for 10+ minutes.

**Forensics.** `audit.jsonl` around the incident shows the resumed session got a fresh
`AgentEntry` (a new id, `orch-1` — `self.seq` is an in-memory-only counter, and `AgentRecord`, the
persisted per-agent roster row, has no `compact_*` fields at all, so no compaction state survives
a restart; this rules out a stale arm carried over from an earlier round or app session). Within
about a minute of resume, something armed `compact_pending` on this fresh entry with no visible
audit trail (arming is silent by design — only resolution audits) — almost certainly an inference
arm, given the session's accumulated size across many testing rounds. Three `compact-pending-
discarded` audits followed, 2-3 minutes apart, each showing an unchanged token reading: D4 held —
this was never a reinjection loop, every cycle resolved correctly. But the user's queued
`request_compact` never got a chance to fire during any of it.

**Root cause: same-tick re-arm racing a queued request.** On the exact tick a discard clears
`compact_pending`, an inference arm can re-satisfy its OWN condition on that SAME tick — manual
detection in particular has no `!currently_quiet` requirement (unlike the banner detector), only
a recency + tail-match check, so it can re-arm immediately after a same-tick discard if the human
was still typing nearby (which the audit trail shows was happening). In the OLD per-agent
iteration order, manual/banner detection ran BEFORE the heuristic/requested-fire check, so a
same-tick re-arm would close the gate before the already-queued, deterministic `request_compact`
ever got a turn — repeatedly, for as long as the inference condition kept recurring.

**Fix 1 — reorder: the loomux-initiated fire-check now runs FIRST**, immediately after the
pause-check, before manual detection, banner detection, and escalation. A queued request is
deterministic and already-decided; an inference arm is a guess. Giving the deterministic check
first refusal means a fresh discard always lets a queued request through before any inference arm
can reclaim the pane that same tick. One side effect: escalation's own "set `compact_requested` on
the agent's behalf" auto-request (previously read by the fire-check in the same tick) now takes
one additional tick to fire — harmless, since the escalation-notice split above only ever needed
"not the same tick as the notice," never "the very next tick" specifically.

**Fix 2 — `ARM_PENDING_TIMEOUT_MS` (5 minutes, symmetric to `REINJECT_CONFIRM_TIMEOUT_MS`)**: a
`compact_pending` arm that never reaches a busy-then-quiet resolution at all (a stalled agent, a
compaction that never actually starts) is now force-abandoned — audited (`compact-arm-timeout`),
latch released — rather than wedging the state machine forever. `AgentEntry.compact_pending_
armed_ms` tracks when the CURRENT arm started (set at all three arm sites, cleared on any
resolution — discard, handoff into the reinjection-confirmation phase, or this timeout itself).
Checked BEFORE the `currently_quiet` gate, since a stuck arm might never go quiet at all.

Regression coverage: `compact_nudge_tick_lets_a_queued_request_win_the_race_against_a_same_tick_
inference_rearm` pins the actual root cause (verified red-before-green by temporarily reverting
the arm-site ordering). `compact_nudge_tick_times_out_a_stuck_arm_that_never_reaches_a_busy_then_
quiet_resolution` and `compact_nudge_tick_arms_cleanly_via_request_compact_after_an_arm_timeout`
cover the arm-pending-timeout contract (verified red-before-green against a neutered timeout
check). Two pre-existing escalation tests were updated for the one-tick delay the reorder
introduces.

### Round 7: model-aware context window, and inference-arm self-echo/cooldown

Demo round 4 succeeded on the core promise (real compact → unprompted re-grounding → ledger
phrase survived → clean recovery), but surfaced two defects in the round-6 lifecycle-panel
instrumentation.

**(1) Wrong denominator for a large-context model.** The lifecycle gauge showed 26% (52,335
tokens) while the CLI's own `/context` showed the same tokens as ~5% — the escalation percent and
the panel's display both divided by a hardcoded 200K context window while the agent ran Opus,
which (in the reporting deployment) runs a 1M-token tier. Investigated what's authoritative and
cheaply available: the Claude transcript does NOT expose a context-limit field directly, but it
DOES expose the model id per turn (`message.model` — already read by `usage::latest_context_
tokens` on its way to the token count, and by `parse_claude_transcript` for cost pricing). This is
more authoritative than block config for the purpose (it reflects what's ACTUALLY running,
immune to config drift), but Claude's real context tier is ultimately a per-request API setting
the transcript doesn't fully pin down — so the model id is a best-effort signal, not a guarantee.

Fix, in one shared place:
- `usage::claude_context_window_tokens(model: Option<&str>) -> u64` — matches by substring the
  same way `price_for` matches for pricing. Opus is the one family with concrete, user-reported
  evidence of a 1M-token tier; everything else, and an absent/unrecognized model, falls back to
  `usage::DEFAULT_CLAUDE_CONTEXT_WINDOW_TOKENS` (200K, unchanged). Erring toward the SMALLER
  window on an unrecognized model is the safe direction: reading a HIGHER percent than reality
  nudges toward compacting SOONER, never later.
- `Guardrails.context_window_tokens_override: Option<u64>` — an explicit human escape hatch that
  wins outright over the model guess, for a deployment where it's wrong. Persisted in group.json;
  no live setter or launcher field this round (same precedent as `max_spawns_per_hour`, which is
  also create-time/hand-edit-only today) — set at launch or by editing an existing group's
  group.json.
- `effective_context_window_tokens(override, model) -> u64` — the single combining function BOTH
  `agent_context_percents` (the escalation threshold) and `group_summary` (the lifecycle panel)
  now call, closing the exact gap the user flagged: before this fix each read the flat constant
  independently, so the escalation threshold was firing ~5x too early for any agent actually
  running a larger-context tier, silently, with no visible symptom other than "unexpectedly early
  escalation notices."
- `usage::latest_context_model`/`CompactionSignal.model` — the transcript reading, sharing
  `latest_context_tokens`' exact "which turn is latest" definition (`latest_real_assistant_turn`)
  so the two can never disagree about which turn they're each reading. Cached on `AgentEntry.
  last_context_model` from `run_compact_nudge`'s own read (not threaded through `compact_nudge_
  tick` itself — nothing in the state-machine DECISION logic needs it, so keeping it out of that
  function's parameter list avoided ~50 test-call-site edits for what is purely a display concern).

**(2) Self-echo spurious inference arm.** Post-recovery, the panel showed a fresh inference arm
"awaiting evidence (unconfirmed)" with no real compaction underway. Forensics (the user's own
disambiguating hypothesis, confirmed as the general mechanism regardless of which detector):
`human_typed_compact_detected` scans the WHOLE bounded output tail for a standalone `/compact`
token, not just fresh growth, and `auto_compact_banner_detected` can match text loomux itself
pasted (the reinjection notice quotes the role instructions verbatim, which may itself describe
this feature). Either detector can misread an ECHO of loomux's own recent activity — a `/compact`
paste or a reinjection notice still sitting in the tail — as fresh evidence, especially once an
UNRELATED human keystroke satisfies the recency-only gate manual detection used to rely on. D4
held both times (every arm resolved to a correctly-audited discard, never a loop) but the noise
is conceptually wrong, and its repeated arm/discard cycling is the same shape #410 (above) closes
separately.

Fix: `AgentEntry.compact_inference_guard_until_ms` — INFERENCE arms (banner, manual detection;
NEVER the trusted arm, which infers nothing) may only arm while `now >= this`. Extended (never
seeded at construction — see the field's doc for why a "distrust every fresh session" grace period
isn't this principle) in two places: (a) **provenance** — a CONFIRMED delivery `from == "loomux"`
(a new field on the existing `DeliveryOutcome`/`DeliveryConfirmation`, captured from `deliver_
prompt`'s own `from` parameter) extends the guard past that delivery's `submit_sent_ms` — loomux's
own paste can never satisfy loomux's own detectors, whatever it pasted; (b) **post-resolve
cooldown** — any terminal `compact_pending` resolution (discard, arm-timeout, reinjection
confirmed, reinjection abandoned) extends the guard past `now` — the immediate post-compact
conversation window. Both extend the SAME field (`.max`, never shortens), so the fix holds
regardless of which mechanism a given false positive traces to.

Regression coverage: `compact_nudge_tick_never_reads_its_own_compact_paste_echo_as_a_manually_
typed_one` pins the user's own disambiguating scenario. `compact_nudge_tick_suppresses_an_
immediate_rearm_of_the_same_false_signal_within_the_cooldown` (and its "a genuine new signal
after the cooldown clears still arms" extension) plus `run_compact_nudge_reads_a_model_aware_
window_from_the_real_transcript` / `..._honors_an_explicit_context_window_override` — all
red-before-green verified. D4's own test (`..._never_loops_when_the_false_signal_repeats_every_
cycle`) was updated to clear the cooldown between its three cycles (the core "zero reinjections,
ever" invariant is unaffected).

**Filed, not implemented this round:** the testbed agent observed that an orchestration-RESTORE
kickoff doesn't embed the directive ledger the way a `/compact` reinjection does — filed as
[#411](https://github.com/willem445/loomux/issues/411), a known scope boundary (a different code
path, `spawn_agent_ex`'s resume branch, not `compact_nudge_tick`).

### #416/#417: a durable system-prompt contract, and compact hooks as a trusted evidence source

Everything above this point hardens **detection and recovery**: infer that a compaction
happened (banner, token drop, `compact_boundary` marker) and re-inject the role contract +
ledger once it's confirmed. #416 and #417 attack the two remaining soft spots that survived
seven review rounds of that machinery: the contract itself never lived anywhere structurally
durable, and every detection path was inference, never direct evidence. Neither issue
restructures the #329 state machine (rev-48/50 judged the core sound) — both land as new
capability at its existing seams.

**#416's actual gap, found by re-deriving the history rather than assuming it.** loomux
already migrated away from `--append-system-prompt-file` once, in #222/#105: `claude --agents
'<json>' --agent <id>` replaced it, because it lets loomux synthesize a persona inline with zero
repo files and zero trust problem (see `profiles.rs`'s module doc and `persona_inject`'s doc).
The natural assumption is that this closed #416 already — it didn't. Reading `persona_inject`
before this change: `--agents` only ever carried a **repo-authored persona's** text
(`ResolvedPersona.text`), and only when a block had one (`prompt:`/`profile:` in
`.loomux/workflow.yml`). The built-in role contract — `mechanics_core` plus the class template,
the actual "you are a worker, here is `report()`, here is the git discipline" text — reached an
agent exactly one way: the kickoff prompt says "First read your role instructions: `<path>`",
and the agent's own `Read` tool call puts that text into the conversation as a **tool result**,
not the system prompt. That's a plain conversation turn, exactly as compressible by a summarizer
as anything else — which is what the user's live v0.10.0 incident actually proved: not that
`--agents` was broken, but that the default roster (the common case — no workflow file) never
used it for the one thing that mattered, and a workflow-customized block's `--agents` payload
carried only the *persona*, never loomux's own mechanics on top of it.

The fix is additive to the existing mechanism, not a second one: `block_contract_text`
(`mod.rs`) unifies `render_block_instructions`'s output (the exact bytes the instructions FILE
gets — refactored out of `write_block_instructions` so file and system-prompt payload can never
diverge) with a configured persona, folded in as an addendum (`mode: append`) or alongside the
non-overridable mechanics core (`mode: replace`) — the same composition the file/kickoff pair
already did, just unified into one string. `persona_inject` now emits `--agents`/`--agent` for
**every** Claude block, persona or not — the trust-root orchestrator included (`--agents` now
always appears on its command line too; what stays absent is repo TEXT, never the flag itself —
see `a_repo_file_can_never_author_the_orchestrators_persona`, updated rather than weakened). One
sharp-edged bug surfaced immediately doing this: loomux's own template prose is full of literal
apostrophes ("don't", "aren't") that were never a hazard while this text only ever reached an
agent via a file read — riding inside `--agents`'s single-quoted shell token, an unescaped `'`
closes that quote early, exactly the hazard `workflow::sanitize_persona` already exists to
neutralize for repo personas. Fixed by sanitizing at the same point personas already are, in
`persona_inject`, never at the source (the instructions file keeps real apostrophes — nothing
about `write_block_instructions`'s output changed, so the `tests/fixtures/pre222` golden is
untouched).

**Per-CLI capability matrix (#416, ORIGINAL — the Claude row is superseded, see
"Round #417 correction 6" immediately below; left as-is rather than silently edited,
per this doc's own honesty convention):**

| | system-prompt mechanism | default roster (no persona) | workflow persona |
|---|---|---|---|
| Claude | `--agents '<json>' --agent <id>` | contract rides inline (new) | contract + persona, one payload (new: contract was previously absent) |
| Copilot, user-authored `.github/agents/*.md` | native `--agent <name>` | n/a | unchanged — `--agent` still points at the exact file loomux read (`profiles::handle_resolves_to`); mechanics-core coverage for this ONE case is still kickoff/file-read only (documented residual gap below) |
| Copilot, no persona / inline `prompt:` | generated `--agent loomux-<group>-<block>` file | contract rides via generated file (new) | contract + persona in the generated file (new: previously nothing for no-persona, kickoff-only for inline `prompt:`) |

Copilot's `--agent <name>` resolves a NAME against a fixed directory precedence
(`~/.copilot/agents` → repo `.github/agents/` → org), confirmed against current upstream docs —
never an arbitrary path. loomux was already careful never to write a generated persona into the
repo's own `.github/agents/` (that would dirty the user's git tree with a file they didn't
author — `profiles::is_copilot_native`'s reasoning). `~/.copilot/agents` is the other end of that
same precedence chain: Copilot's OWN user-level convention, not a loomux invention, exactly as
`~/.claude/agents` is for Claude — so writing loomux's generated wrapper there (`OrchRegistry::
write_copilot_agent_file`, test-overridable via `set_copilot_agents_dir_override`, mirroring
`set_claude_projects_dir`) closes the SAME gap Claude had, for the SAME reason, without touching
anything the user authored. The one deliberately NOT closed this round: a user's own native
`.github/agents/*.md` persona still gets loomux's mechanics core only via the kickoff/file-read
path, not the system-prompt layer — synthesizing a wrapper around a user-authored file would
trade the carefully-reasoned `handle_resolves_to` trust property (the `--agent` flag resolves to
**exactly the file loomux read and kind-checked**, nothing else) for coverage this one case
doesn't have yet. Worth closing, not free, left as follow-up.

**Honesty note, since this is a claim about a CLI loomux doesn't control:** whether a Copilot
custom-agent file's content survives Copilot's OWN compaction/summarization structurally (the
way `--agents` unambiguously is Claude's system-prompt layer) is **not confirmed** by Copilot's
docs — no source describes where custom-agent instructions live in Copilot's prompt
architecture relative to a `/compact` event. This is the best available mechanism (a system-level
custom-agent flag beats a conversation-turn file-read on priors), not a proven guarantee — a
gap to note honestly rather than paper over, and easy to revisit if Copilot's own docs firm up.

**Round #417 correction 6 — a live demo blocker: `--agents` put the durable contract on argv,
and Windows CreateProcessW has a hard 32,767-character command-line limit.** The user's own
demo, launching a Claude orchestrator in this worktree, failed outright with "loomux: failed to
start shell" / a `CreateProcessW` error. The screenshot showed the FULL orchestrator contract
(mechanics core + template, many KB) embedded inline in the `--agents` JSON, routed through the
`pwsh.exe -Command` shell fallback. Root cause, confirmed rather than assumed: `--agents` only
ever carried a SHORT repo persona before #416 (see the `## #416's actual gap` paragraph above)
— the reviews that shipped #416 verified that token's QUOTING (the apostrophe/ASCII-escaping
work described above) but never its LENGTH, because nothing before #416 put more than a few
hundred bytes of persona text there. #416 widened the payload to loomux's OWN template prose on
EVERY block, unconditionally — categorically bigger, and eventually big enough to blow the
limit. The failing path was the `pwsh -Command` fallback (whose extra escaping layer inflates
an already-long line further), but the direct-`CreateProcessW`-spawn path is subject to the
identical 32,767-character hard limit — so the fix could not depend on which spawn path runs;
it had to remove the multi-KB payload from argv on BOTH paths.

**The fix, per Claude's own CLI reference (code.claude.com/docs/en/cli-reference) and its
sub-agents doc (code.claude.com/docs/en/sub-agents), read in full for this round: the contract
now travels by FILE, mirroring the Copilot precedent, not argv.** The reference confirms
`--agent <name>` "activates a custom agent by name" and, per the sub-agents doc's own "Run the
whole session as a subagent" section: passing `--agent <name>` alone "start[s] a session where
the main thread itself takes on that subagent's system prompt... replac[ing] the default Claude
Code system prompt entirely" — when `<name>` resolves against `.claude/agents/` (project) or
`~/.claude/agents/` (user), both scanned automatically, no `--agents` JSON required at all. This
is the EXACT "native custom-agent flag, file-backed" shape Copilot's `~/.copilot/agents`
precedent already used — round 6 makes it true for Claude too, closing the gap the ORIGINAL
`--agents '<json>'` choice opened (see `write_claude_agent_file`, `claude_agents_dir`,
`PersonaInject::claude_agent`'s docs in `mod.rs`):

- **Primary path**: `write_claude_agent_file` writes a loomux-generated, uniquely-handled
  (`loomux-<group>-<block>`, same convention as Copilot's) `~/.claude/agents/<handle>.md` file
  carrying the FULL contract as its markdown body, with `name`/`description` frontmatter
  (`description` is REQUIRED by Claude's own schema, unlike Copilot's generated file, which has
  none — `yaml_double_quoted` quotes it for YAML safety, since a repo persona's description is
  free text and not guaranteed YAML-safe on its own). `--agent <handle>` alone activates it —
  `--agents` is GONE from loomux's command lines entirely, for every CLI, on every path.
- **Fallback path** (the directory is unwritable — fail-open, matching every other hook/shim
  path in this codebase): `--append-system-prompt-file <path>`, pointed at the SAME instructions
  file `write_instruction_files` already reliably writes to the group's own state dir — no
  second file to invent or clean up. The reference confirms this flag "load[s] additional system
  prompt text from a file and append[s] to the default prompt." Still launch-time system-prompt
  construction, not a conversation-history artifact — `contract_on_system_layer` stays `true` in
  this path too, unlike Copilot's own write-failure fallback (which has no such flag and
  genuinely degrades to a kickoff-only paste, so `contract_on_system_layer` correctly goes
  `false` there). Neither Claude fallback ever re-embeds the contract on argv.
- **`--append-system-prompt-file` was #105's own mechanism, abandoned in #222/#416 purely
  because `--agents` was newer and native** ("claude gets the native flag rather than
  `--append-system-prompt-file`, which predates the flag" — `profiles.rs`'s own module doc,
  reread rather than assumed for this round) — never because the file-based flag itself had a
  functional problem. That reasoning doesn't disqualify it as today's FALLBACK: the content
  riding it is now loomux's own trusted contract (not a repo persona needing the trust story
  `--agents` was chosen for), and a file has no length limit remotely close to argv's.
- **`sanitize_persona`'s apostrophe-mapping and `ascii_escape_json` — the two safeguards that
  existed specifically for the single-quoted `--agents` shell token — are now inert (the first,
  kept as defense-in-depth) or dead (the second, removed entirely).** Neither a generated FILE
  nor `--append-system-prompt-file`'s path argument is a raw shell token carrying persona TEXT,
  so the shell-quoting hazard those functions protected against no longer exists on the Claude
  path. See `workflow::sanitize_persona`'s doc for the full reasoning on why one was kept and the
  other removed rather than both left orphaned.

**Belt-and-braces: a pre-spawn command-line length guard, independent of the file-based fix.**
`command_line_length_guard` (`mod.rs`) checks the STRUCTURED argv form — which both spawn paths
compile from, so one check covers both — against `WINDOWS_COMMAND_LINE_SAFETY_LIMIT` (28,000,
comfortably under the documented 32,767, leaving headroom for the shell fallback's own quoting
inflation). Wired into both spawn call sites (`spawn_agent_ex` and `register_orchestrator_pane`)
right after `build_agent_argv`, returning a loud, diagnostic `Err` naming the oversized
argument's index and byte length — never again the unreadable `CreateProcessW` wall the user's
demo hit. This is NOT how the argv-length bug is fixed (the file-based mechanism above is that);
it is the backstop for a FUTURE regression that puts something large on argv again, on either
CLI — checked, not assumed, to hold for Copilot too: nothing in its command-builder branch ever
pushes free text onto argv (`--agent <name>` is always a short handle, either a native
`.github/agents/*.md` file's `name:` frontmatter — itself constrained to short lowercase-and-
hyphens tokens — or loomux's own short generated handle), so Copilot never had this bug, and the
guard confirms rather than assumes that going forward.

**Updated per-CLI capability matrix (supersedes the Claude row above):**

| | system-prompt mechanism | default roster (no persona) | workflow persona |
|---|---|---|---|
| Claude | `--agent <handle>` → generated `~/.claude/agents/<handle>.md` FILE (fallback: `--append-system-prompt-file <instructions file>`) | contract rides via generated file | contract + persona, one file (never argv, either way) |
| Copilot, user-authored `.github/agents/*.md` | native `--agent <name>` | n/a | unchanged — `--agent` still points at the exact file loomux read (`profiles::handle_resolves_to`); mechanics-core coverage for this ONE case is still kickoff/file-read only (documented residual gap above) |
| Copilot, no persona / inline `prompt:` | generated `--agent loomux-<group>-<block>` file | contract rides via generated file | contract + persona in the generated file |

**Tests:** `a_thirty_kb_contract_still_produces_a_short_command_line` reproduces the actual
regression shape (a 30KB+ persona) and pins both halves of the fix — the command line stays
short AND the full contract still reaches the generated file;
`command_line_length_guard_fails_loudly_on_an_oversized_argument` pins the backstop against both
a single oversized argument and many small ones summing past the limit;
`claude_agent_file_write_failure_falls_back_to_append_system_prompt_file` forces the fallback
for REAL (a regular file occupies the target directory path, so `fs::create_dir_all` genuinely
fails) and pins it lands on the SAME instructions file, never argv;
`end_group_reclaims_generated_claude_agent_files` mirrors N4's Copilot cleanup test for Claude's
own generated files. Every pre-existing test asserting the OLD `--agents '<json>'` shape (the
golden default-roster pin, the trust-root security tests, the drift-guard matrix, the
name-cannot-break-the-payload test) was updated to the new shape rather than deleted — the
security PROPERTY each one pins ("no repo text reaches the trust root", "ids/names can't break
structurally out of whatever they ride in") still holds, now checked against the generated
file's content instead of the command line, since the command line no longer carries it.

### #502: the generated agent files were never written where they belonged

**The incident.** `~/.claude/agents/` had accumulated **1,219 loomux-generated agent definition
files (67MB)** against **13** live group dirs. Claude Code loads every agent definition's
`description` into the session's agent roster and caps the aggregate; the pile tripped that cap
outright (`Agent descriptions are over the 15.0k-token limit (~15.6k tokens)`) and bloated every
session's context until a human swept it by hand — after which files for long-dead groups
**reappeared within minutes**.

**Split with #464, stated plainly.** #464 shipped the *reclaim* half — `sweep_orphaned_agent_files`,
a one-shot startup sweep of files no live group claims, plus a static pin that no test may
construct a bare registry. This section is the *cause* half, and the two are deliberately not the
same fix: a sweep reclaims what was already written, and nothing in it stops the writing. An
earlier revision of #502 carried its own second sweep and its own ownership-marker scheme; both
were dropped rather than merged, because two implementations of one mechanism is a defect in
waiting, and requiring a marker would have stopped #464's sweep reclaiming the very legacy
orphans it exists for. #464's sweep is kept verbatim, including its "if enumeration fails, delete
nothing" rule (its review B3) — which #502's own delete path now also applies, so the two hold
one opinion instead of two.

#### Where the files actually came from

The names identified the writer. A group id is `<repo-dir-name>-<hash>` (`group_id_for_repo`),
and the reappearing files were `loomux-tmp<random>-<hash>-*` and `loomux-repo-<hash>-*` —
`tempfile::tempdir()` directory names, and the fake `C:/tmp/repo` path used throughout the test
suite. One was `…-rev-security`, a block id only `tests/workflow.rs` declares. **The writer was
loomux's own test suite**, not the running app.

That last part is a *verified* negative, not an assumption: `write_claude_agent_file` and
`write_copilot_agent_file` are reachable only through `persona_inject`, whose only two callers
are `spawn_agent_ex` and `register_orchestrator_pane` — both spawn paths. There is no periodic
refresh, no ensure-definitions pass, and no restore-index iteration that writes an agent file.

`claude_agents_dir()`/`copilot_agents_dir()` resolved the user's real home for **every** registry,
and only the shared test helper pointed them elsewhere — so every test that built its own registry
and spawned an agent minted a real multi-KB file in the developer's home that nothing ever
deleted. Reproduced in one command: a single pre-existing test wrote two new files into the real
agents dir.

#### One containment predicate, three paths

The rule is that **only the registry that owns the user's live orchestration state may resolve a
directory inside the user's home** — `is_live_registry()`, which is `self.root == default_root()`,
the root `lib.rs` constructs exactly once. Every other registry is contained inside its own root.

It lives in one predicate because the first two attempts open-coded it per site, and the third
site was then missed:

- `user_cli_dir` — the generated agent dirs.
- `copilot_home_dir` — Copilot's hook config **and** its permission grants (since #802, both
  `permissions-config.json` and the legacy `config.json`/`trustedFolders`; see "Copilot's two
  permission surfaces" below).

That third one, `pre_trust_copilot_folder`, is worth naming: it grants the spawn's workspace path
in the user's real `~/.copilot`. It is the worst of the three to leak on two counts the others
don't share — it is a **security** setting (it suppresses Copilot's folder-trust prompt and
approves loomux's MCP tools), and its entries are per-workspace **paths**, so a suite spawning
into fresh temp dirs appends a new entry every run and grows without bound. The observed config
had collected 36 of them. It was missed by an audit that *did* list it, because it was a
free function with no registry to ask: the very property that made it unfixable by the rule being
applied is what excused it from the rule. **A rule copied per site is a rule the next site
forgets.**

#475's thread-local trust seam still takes precedence where a test sets it: fixturing it is an
explicit statement about where the write should go, and an explicit statement beats a structural
default. Containment is what catches everything that never makes one.

`compact_hook_dir` was already root-relative and needed nothing.

**Failure direction is safe throughout.** If the app's root ever stops being `default_root()`,
generated files land in that root instead of the user's home, and `persona_inject`'s existing
write-failure fallbacks (`--append-system-prompt-file` on Claude, kickoff text on Copilot) still
deliver the contract. Nothing silently writes where it shouldn't.

#### Writers refuse a group that does not exist

`group_state_exists` — both writers refuse to write for a group with no state dir, asked against
the same registry the reclaim path consults, so a write and a reclaim can never disagree about
whether a group exists. This is the general form of "only write for groups that actually exist":
*any* path that reaches a writer with a dead group id — a stale roster, a restore index, some
future periodic refresh nobody has written yet — fails to resurrect it, rather than only the path
that does so today. `create_group` makes the directory before any spawn can reach a writer, so a
live group always passes.

#### Teardown reclaims by ownership, not by member list

`end_group`'s reclaim (`reclaim_group_agent_files`) scans the agent dirs and asks who owns each
candidate, replacing a per-MEMBER name list that structurally could not see a file whose block a
roster change retired, or whose member entry was pruned while the file stayed on disk. Ownership
is a property of the FILE, not of who is still in the roster.

`owning_group` resolves by **longest** match, because nothing stops one group id from being a
prefix of another: with `X` and `X-extra` both live, `loomux-X-extra-worker.md` is X-extra's
worker file, not X's file for a block named `extra-worker`. The filename alone cannot decide —
both readings are well-formed — so ownership is settled against the group registry and resolved
toward the more specific claim. First-match logic would let `end_group("X")` delete a LIVE group's
file.

An unenumerable registry aborts the reclaim entirely, for the same reason #464's sweep abstains:
ownership is decided by asking which live group *claims* a file, so an empty group list says
"nothing claims anything". The general lesson, and the reason this was worth a fix rather than a
shrug: **a conservatism argument that rests on "we only delete what we can prove nothing claims"
is void in exactly the states where the proof is unavailable**, and those are the states worth
writing down.

#### Description length is a shared budget

The cap Claude Code enforces is on the AGGREGATE of every agent description, so verbosity in one
repo's persona is spent from every session's ceiling. loomux's own descriptions were already terse
(the block id), so accumulation, not verbosity, caused this incident — but a repo-authored
persona's `description:` is unbounded free text flowing straight into the generated file, so it is
clamped (`clamp_agent_description`, `GENERATED_AGENT_DESCRIPTION_MAX_CHARS` = 160, counted in
characters since persona text is not guaranteed ASCII). The persona's own TEXT is untouched — only
the roster-facing one-liner.

**Generic, per CLAUDE.md constraint 8**: this keys off loomux's own group registry and its own
naming — no CLI-version, machine, or user assumptions beyond the agent-dir convention loomux
already owns in order to write these files at all.

#### Tests

`a_registry_outside_the_default_root_never_writes_into_the_users_home`,
`a_throwaway_registry_never_rewrites_the_users_real_copilot_hook_config` and
`a_throwaway_registry_never_writes_the_users_real_copilot_trusted_folders` pin the three contained
paths. Each asserts the CONTAINED file exists rather than that the real one is absent — the real
one legitimately exists on a developer's box, so its presence would prove nothing either way — and
each deliberately builds its registry the **unguarded** way, which is why #464's
registry-construction pin grows a narrow, greppable opt-out token rather than a raised count: a
test whose subject IS bare construction must build one, and every other test still routes through
the helper.

`a_dead_group_never_gets_its_agent_file_re_minted` pins the writer guard;
`end_group_reclaims_a_generated_file_no_member_entry_accounts_for` and
`end_group_leaves_a_generated_file_a_more_specific_live_group_owns` pin the reclaim and the
longest-match rule; `an_unreadable_group_registry_stops_end_group_reclaiming_anything` pins the
abstain rule; `a_verbose_persona_description_is_clamped_in_the_generated_agent_file` pins the
clamp.

That last enumeration test earned a note of its own: driving it through `end_group` produced a
test that passed for the wrong reason. `end_group` kills its members first, `mark_dead` audits,
and `append_audit` does `create_dir_all(root/<group>)` — so a merely-deleted root is **recreated**
before the reclaim ever runs, and enumeration quietly succeeds. It is driven at the reclaim
directly instead, against a root that is a regular file and therefore can never be enumerated or
recreated.

### #417: compact hooks as a TRUSTED evidence source

**Correction (this section originally claimed Copilot has no compaction hooks at all — that
was wrong, and the record should say so plainly rather than quietly editing history.** An
earlier round asserted this based on Copilot's changelog and an open feature-request issue that
turned out to be stale/misleading; the user supplied the authoritative reference —
[docs.github.com/en/copilot/reference/hooks-reference](https://docs.github.com/en/copilot/reference/hooks-reference)
— which documents 14 hook events including `preCompact`, with a payload nearly identical to
Claude's. Read in full before correcting this section. Copilot IS wired into the hook tier as
of this correction; see below for exactly how much of #417 that covers and how much it doesn't.

Claude Code has a `PreCompact` hook (fires before manual AND auto compaction) and a
`SessionStart` hook with `matcher: "compact"` whose output can inject `additionalContext`
natively.

**Copilot's real event table** (per the reference above): `preCompact` fires with `{sessionId,
timestamp, cwd, transcriptPath, trigger: "manual"|"auto", customInstructions}` — structurally
the same evidence Claude's version gives. **There is no `postCompact` event of any kind** (the
docs are silent on one existing at all), and `sessionStart`'s `source` field is documented as
taking **exactly** `"startup" | "resume" | "new"` — no `"compact"` value, and no other mention
of compaction triggering it. This is a confirmed negative, not an open question: Copilot has a
trusted ARM signal but no trusted CONFIRM/native-re-grounding signal the way Claude's
`SessionStart(compact)` gives one. Per the #413 tier model, Copilot's hook wiring below covers
the ARM half of #417 only; the CONFIRM half already had a trust bypass that doesn't need a
second signal (see the bullet list below) — Copilot does not fall back to the pre-#417
inference tier the way this section originally (wrongly) implied it would need to.

**Provisioning, without touching the user's own settings.** Claude Code's `--settings
<file-or-json>` flag loads an ADDITIONAL settings layer Claude Code itself composes over
project/user settings — so "never clobber the user's own hooks" is a property of the CLI flag,
not something loomux re-implements by hand-parsing and merging their `.claude/settings.json`
(which loomux never touches). The generated hook config lives in its OWN file
(`write_hook_settings_file`, `<agent-id>-hooks.json`) — deliberately SEPARATE from
`--mcp-config`'s file (`write_mcp_config`), rather than one file under two flags: an earlier
revision folded `hooks` into the same JSON `--mcp-config`/`--strict-mcp-config` already point at,
which only stays safe while each flag's reader happens to ignore the other's top-level keys — a
schema assumption neither this file nor Claude Code's own docs pin down, and a future release
tightening either reader's validation would be a silent breakage no local test could catch (rev-4
review, N2). Two small generated files, one per flag, costs one extra `fs::write` per Claude
spawn and removes that risk entirely. Absent a resolvable `sh` interpreter (`resolve_hook_sh`,
mirroring the `.cmd` shim delegators' `winpath::resolve_sh` reasoning — never trusting a bare
`sh` on PATH, which Windows doesn't guarantee), `write_hook_settings_file` returns `None` and
`--settings` is omitted entirely — never pointed at an empty file, fail-open, same policy as a
missing gh/git shim.

The hook `command` for each event explicitly invokes the resolved `sh` against ONE generic
script (`COMPACT_HOOK_SCRIPT`, written once per machine to a `compacthook/` dir sibling to
`ghshim/`) with the event name, this group's state dir, and this agent's id as literal argv —
so the script itself carries zero repo/group-specific text (constraint #8) and no new env var
is needed to tell it which agent it's running for. It does exactly two things, both fail-open
(every path exits 0 — a hook's nonzero exit can block the CLI's own lifecycle event):
`precompact` creates/truncates a marker file at `<group_dir>/hooks/<agent_id>.precompact.json`;
`sessionstart-compact` does the same for its own marker AND prints Claude's native
`additionalContext` JSON shape with a fixed, generic re-grounding line (pointing at the ledger
path by convention, not by reading it — keeping the script free of any ledger-parsing/truncation
logic, which stays Rust-side in `directive_ledger_embed` to avoid two implementations of the same
cap/truncation rule drifting apart).

**Hybrid, argued rather than assumed:** native `additionalContext` is race-free (no delivery/hold
window — the whole class of bug rounds 4-6 fixed) but necessarily minimal (a hook script has no
business re-implementing `directive_ledger_embed`'s cap/truncation logic in shell). Whether
loomux's own tick-driven reinjection ALSO fires depends on which marker confirmed the
compaction (rev-4 review, N3 — see below): a PreCompact-only arm never delivered native
context (only the `sessionstart-compact` script branch emits `additionalContext`), so loomux's
reinjection is the ONLY re-grounding channel and still fires normally; a SessionStart-confirmed
arm already got native re-grounding, so loomux's reinjection is SKIPPED for it — pasting it
anyway would be a duplicate, spending exactly the context tokens native delivery exists to save.
What #417 actually replaces is the ARM/CONFIRM evidence, not the reinjection delivery itself
(which now runs, or doesn't, per the rule above):

- `read_hook_marker_ts` reads a marker FILE's mtime (not a timestamp encoded in its content —
  the generic script never needs portable clock-formatting logic this way). `AgentEntry.
  compact_hook_precompact_seen_ms`/`compact_hook_sessionstart_seen_ms` bookkeep the freshest
  marker already consumed.
- Checked in `compact_nudge_tick`'s per-agent loop, right after the existing rebaseline/growth
  block and before every pre-existing arm site (loomux-initiated, manual, banner) — a FRESH
  PreCompact marker arms exactly like the loomux-initiated path (`compact_pending_trusted =
  true`, no `inferred_compaction_confirmed` gate) and is treated as already busy (the compaction
  itself IS the busy half, whether or not it happened to clear the byte-growth floor). A FRESH
  SessionStart(compact) marker is even stronger — direct proof Claude Code restarted the session
  BECAUSE of a compact — so it arms-and-confirms unconditionally (covers a hook config with only
  SessionStart wired, or a PreCompact marker this tick loop raced past). With the pane already
  quiet (the common case — nothing typed since the hook fired), arm and resolve-into-reinjection
  happen on the SAME tick, closing the gap faster than a banner/manual arm ever could (those
  always need a LATER quiet observation).
- `AgentEntry.compact_pending_evidence: Option<&'static str>` (`Some("hook")`) rides alongside
  `compact_pending_trusted` purely for visibility — audited as its own event
  (`compact-hook-evidence`, distinct from `compact-nudge`/`compact-reinjection`) and threaded
  through `compaction_status`'s new `source` field so the lifecycle chip (`compactionstatus.ts`)
  reads "armed (hook-confirmed)" rather than conflating a direct signal with the
  loomux-initiated trusted arm that happens to look identical in every OTHER field.

**Copilot wiring (correction round).** `compact_hook_cli_supported(cli)` (`claude | copilot`) was
introduced this round as a NEW, broader admission gate for `compact_nudge_tick`'s per-agent loop
— at the time, deliberately kept distinct from `compact_nudge_cli_supported` (which this round
left claude-only), on the premise that Copilot has no `/compact` command to paste. **That premise
was wrong — see "rev-4 review round 4 (Copilot `/compact` correction)" below, which widened
`compact_nudge_cli_supported` itself and collapsed the two gates back into one function**; this
paragraph is left as-is (rather than silently edited) as the record of what this round actually
believed and built. What's still accurate: before THIS round's admission-gate widening, the
loop's own top-of-function gate WAS the narrower `/compact`-paste gate, so a copilot agent never
reached ANY of the loop's logic, hook-evidence included — and once admitted, a Copilot
`preCompact` marker is read and consumed by the EXACT SAME code the Claude `precompact` case
uses — "one mechanism, two writers": the marker-file convention
(`<group_dir>/hooks/<agent_id>.precompact.json`), the `ts >= a.started_ms` freshness gate, and
delete-on-consume are all CLI-agnostic already, so no new state-machine code was needed, only the
gate change plus provisioning.

- **Trust class.** A Copilot `preCompact` marker sets `compact_pending_trusted = true`, the same
  evidence class as Claude's — a hook is equally trustworthy regardless of which CLI's hook fired
  it. Since Copilot has no compact-sourced `SessionStart` (confirmed absent, see above),
  `compact_hook_native_notice_delivered` never gets set for it, so N3's suppression never
  applies — loomux's own reinjection is (correctly) the ONLY re-grounding channel for a Copilot
  compaction, same shape as Claude's precompact-only case.
- **Provisioning is a single, GLOBAL, machine-wide file**, not one per agent or group — the
  opposite of Claude's per-agent `--settings` file. Copilot's own hook-config precedence list
  (policy → repo `.github/hooks/` → **user-level `~/.copilot/hooks/*.json` (or `$COPILOT_HOME/
  hooks`)** → inline settings → plugins) is loaded automatically; loomux writes exactly one
  small, idempotent, always-rewritten file there (`ensure_copilot_compact_hook`,
  `loomux-compact.json`) — never the repo's `.github/hooks/`, for the same reason a generated
  file never lands in `.github/agents/` (#416): it would dirty the user's git tree with
  something they didn't author. Unlike Claude's `--settings` (unverified merge semantics — see
  below), Copilot's docs EXPLICITLY confirm multiple hook-config sources are additive: "When the
  same event appears in multiple sources, all hook entries from all sources are run" — so this
  file is proven, not merely argued, never to clobber a user's own hooks.
- **Self-scoping via env-var presence, not `cwd`/session matching.** Because the config is
  global, EVERY Copilot session on the machine loads it — a human's own, non-loomux session
  included. The brief's suggested fix was matching the hook payload's `cwd` against known loomux
  worktrees, or a registration file the script re-reads. Both were set aside for a THIRD option
  that's simpler and already the codebase's own convention: the hook script (both the `bash` and
  `powershell` command variants, run via `type: "command"`'s two OS-specific fields — Copilot
  itself picks the one for its host OS, so loomux never resolves its own interpreter path the
  way Claude's hook command does) checks for `LOOMUX_GROUP_DIR`/`LOOMUX_AGENT_ID` in its OWN
  inherited environment and no-ops silently if either is absent. This is the EXACT idiom the
  gh/git shims already use (`LOOMUX_GROUP_DIR` unset ⇒ "not a loomux pane, refuse/no-op") —
  reused here rather than invented fresh, and it avoids both alternatives' failure modes: a
  registration file needs extra I/O and a staleness/race window on every hook invocation; a
  static list baked in at generation time goes stale the moment a new group spawns after the
  file was last written. Env-var inheritance has neither problem — it's always current for the
  actual process the hook runs inside. `agent_pane_env` now sets `LOOMUX_AGENT_ID` alongside the
  pre-existing `LOOMUX_GROUP_DIR` on every agent pane for exactly this purpose (Claude's own hook
  command never needed it, since its per-agent `--settings` file bakes the agent id into its own
  argv instead).
- **No payload parsing, deliberately.** The hook script never reads the JSON Copilot pipes to it
  at all — the marker file's mere existence (at `read_hook_marker_ts`'s mtime) is the whole
  signal. This sidesteps a real wrinkle in the reference docs: every event ships in TWO payload
  shapes, camelCase (`sessionId`, `transcriptPath`) and a VS-Code-compatible snake_case one
  (`session_id`, `transcript_path`, plus a `hook_event_name` field) — a script that needed to
  read `trigger`/`sessionId` would have to handle both. Since nothing here needs the payload's
  content, that casing distinction never has to be gotten right, and there is no parser in
  loomux's own code to test either way (see `ensure_copilot_compact_hook_writes_an_additive_
  generic_precompact_entry`'s own note on this).
- **Cleanup and orphans.** Unlike the per-group `--settings`/generated-agent files, this ONE
  config file is not per-group at all, so there's nothing to reclaim per `end_group` — it stays
  installed for the lifetime of the machine's Copilot install, the same way the gh/git shims and
  the Claude hook script itself are never uninstalled per group either. No new orphan-sweep
  surface here.

**Demo-script checks, since the docs' own precision doesn't replace live verification:**
1. Trigger a real Copilot compaction and confirm the `preCompact` marker actually appears under
   `<group state dir>/hooks/<agent-id>.precompact.json` and the lifecycle chip reads
   "armed (hook-confirmed)".
2. Watch what `sessionStart`'s `source` value actually reads immediately after that compaction,
   in practice — the docs say `startup|resume|new` with nothing indicating compaction, but CLI
   behavior can outrun docs; if a live session ever shows an unexpected value there, that is the
   signal a native-re-grounding path could be wired for Copilot too, symmetric with Claude's.
3. **(#417 correction round 5)** For that same real Copilot compaction, confirm the generated
   `~/.copilot/agents/loomux-<group>-<block>.agent.md` content is STILL what the agent acts on
   afterward — the one soft spot the docs don't specifically pin down (they confirm "original
   user instructions" survive compaction generally, not an `agent.md` FILE'S specific region of
   the prompt architecture). If it isn't, `contract_on_system_layer` is wrong for the generated-
   wrapper path and the slim notice is under-informing that agent — the signal to revert this
   one path to verbose, not evidence against the Claude side (whose `--agents` system-prompt
   durability is separately and more directly confirmed).

Nothing about the resolution logic itself — busy-then-quiet, the confirm gate, the
delivery-confirmation retry/abandon bounds, the per-agent arm-timeout — changed; the hook is
purely a new, stronger way to reach the SAME `compact_pending`/`compact_seen_busy`/
`compact_pending_trusted` triple every pre-existing arm site already writes.

**Merge semantics — updated with the authoritative reference (rev-4 review round 3).** Claude
Code's own hooks reference (code.claude.com/docs/en/hooks), read in full for this round, settles
the general question this section originally hedged on: hook configuration from every documented
source — user (`~/.claude/settings.json`), project (`.claude/settings.json`,
`.claude/settings.local.json`), managed policy settings, plugin `hooks/hooks.json`, and
skill/agent frontmatter — is **additive, not replacing**. Two direct quotes: "All matching hooks
run in parallel, and identical handlers are deduplicated automatically" (dedup is by exact command
string/args or URL — never by event), and "When a plugin is enabled, its hooks merge with your
user and project hooks." Nothing in the reference describes any source REPLACING another's
entries for the same event.

**The one residual, stated precisely (not a general hedge anymore):** the reference's own
location/precedence table does not list `--settings` as a source at all — it documents the six
sources above, and loomux's generated file rides on a CLI flag the table is simply silent about.
Given every OTHER documented source is additive, and `--settings` is Claude Code's own mechanism
for layering settings in from outside those standard file locations, the strong prior is that it
composes the same way — but this ONE specific gap (an undocumented flag's exact interaction with
an otherwise-fully-documented additive model) is what the "never spawn a real agent CLI to test"
constraint prevents this change from closing empirically. The demo script asks for a quick live
confirmation of this specific point, not a full investigation of merge behavior in general (that
question is now answered by the docs). If `--settings` turns out to be the one source that
doesn't compose the way every other one does, the consequence is not cosmetic: a user's own hook
for the same event (say, a `PreToolUse` denial) would stop firing the moment loomux's file is
added — worth a quick explicit check, not assumed either way.

**rev-4 review round — one blocking fix, three follow-on hardening items:**

- **B1 (blocking): a live restart could arm a spurious, ungrounded reinjection.** Marker files are
  never deleted on their own, `AgentEntry.compact_hook_*_seen_ms` is in-memory (resets to `None`
  on every restart), and agent ids come from an in-memory counter — so a fresh boot can mint an id
  a PREVIOUS process already used, while that old process's marker file sits untouched on disk.
  The very first tick after such a restart would have read it as "fresh evidence" (`ts >
  None.unwrap_or(0)` is true for any real mtime), arming TRUSTED with no compaction having
  happened. Fixed with three layers, since this is the TRUSTED tier and a false positive here is
  worse than one in the inference tiers: (1) `ts >= a.started_ms` — a marker can only be evidence
  for a compaction that happened during THIS agent's own lifetime (`>=`, not `>`: both timestamps
  are millisecond-resolution real wall clock, so a marker written in the same millisecond an
  agent started is still legitimately its own); (2) delete-on-consume — an actually-used marker is
  removed from disk immediately, independent of whether the in-memory bookkeeping survives a
  future restart; (3) a regression test (`compact_nudge_tick_ignores_a_hook_marker_older_than_
  the_agent_itself`) simulating the exact sequence — a fresh `AgentEntry`, the same agent id, an
  old marker already present.
- **N2: split the shared `--mcp-config`/`--settings` file.** Covered above — two small generated
  files now, removing the schema-drift risk of one file serving two flags with two different
  readers.
- **N3: suppress loomux's own reinjection when native re-grounding already landed.** Covered
  above — `AgentEntry.compact_hook_native_notice_delivered` tracks specifically whether the
  SessionStart hook (the one script branch that emits `additionalContext`) confirmed THIS pending
  cycle; when it did, the resolution audits `compact-reinjection-skipped-native` and clears
  straight to a terminal state instead of pasting a duplicate notice. A PreCompact-only arm (no
  SessionStart marker seen) is unaffected — loomux's reinjection remains the only channel and
  still fires, proven by its own test
  (`compact_nudge_tick_a_precompact_only_arm_still_gets_loomuxs_own_reinjection`) alongside the
  positive case (`compact_nudge_tick_treats_a_sessionstart_hook_marker_as_an_immediate_confirm`,
  updated for the new terminal-resolution shape).
- **N4: reclaim generated Copilot agent files on group end.** Without this, a #416-generated
  `~/.copilot/agents/loomux-<group>-<block>.agent.md` outlives the group it was written for,
  accumulating forever and cluttering the user's real Copilot agent list with dead groups' names.
  `end_group` now sweeps every member's handle (`end_group_reclaims_generated_copilot_agent_
  files`), best-effort and harmless to attempt even for a Claude-only group (no handle was ever
  written for it). **Deliberately NOT built this round:** a startup sweep reconciling orphans from
  a group that never reaches `end_group` at all (a crash, or state deleted out from under loomux).
  A stray tiny markdown file the user can delete by hand is a cosmetic cost, not a resource or
  security one — a reconciliation sweep is real, if modest, additional complexity (enumerating
  every group's state to know which handles are still legitimate) for a narrow, self-correcting-
  by-hand failure mode, so it's left as a deliberate follow-up rather than built speculatively.
  **Built after all, in #464** — the "narrow, self-correcting-by-hand" framing above turned out to
  undercount the source: the ORCHESTRATION TEST SUITE spawns agents through the exact same
  `write_claude_agent_file`/`write_copilot_agent_file` path and, being unit tests, essentially never
  calls `end_group` — every `OrchRegistry::new(...)` in `tests/orchestration.rs`/`tests/workflow.rs`
  that skipped the test-only `claude_agents_dir_override`/`copilot_agents_dir_override` (the
  "relaunch" pattern: a second registry built against the same or a related state root to simulate
  loomux restarting, common across the persistence tests) fell straight through to the REAL
  `~/.claude/agents`/`~/.copilot/agents`. 1,111 and 161 such stray files were found on a real dev
  machine — not cosmetic at that volume, and not remotely limited to crashes. Fixed two ways:
  test-side, every registry construction across both files now routes through a `relaunch_registry`
  helper that always applies the overrides (statically pinned by
  `no_registry_construction_bypasses_the_test_agent_dir_overrides` in each file — it greps its own
  source for a raw `OrchRegistry::new(...)` outside that helper); product-side,
  `OrchRegistry::sweep_orphaned_agent_files` now runs once at launch (`lib.rs`'s `setup`, alongside
  `start_disk_monitor` et al.) and reclaims any `loomux-<group>-*` file whose `<group>` no longer has
  a directory under this registry's own `state_root()` — conservative by construction: only
  `loomux-`-prefixed entries are ever considered, matched against known groups by `-`/`.`-delimited
  prefix (not a naive `-`-split, since both a group id and a block id can contain `-` themselves),
  and every reclaim is breadcrumbed (`fixture-sweep`). See `sweep_orphaned_agent_files_reclaims_
  orphans_but_refuses_a_live_group` (`tests/orchestration.rs`) for the orphan-vs-live-group proof.
  This does NOT reach the OTHER #464 leak (leaked `%TEMP%` git-worktree *directories*, a much larger
  volume — 2,438 growing to 2,702 on the same machine): that one's root cause was `real_repo()` (and
  `workflow.rs`'s `Repo::git_init()`) binding the fixture's git repo directly to a bare
  `tempfile::tempdir()`, so `git_worktree_add`'s cut worktree — created at `<repo's-parent>/<repo-
  name>-worktrees/<name>`, a directory SIBLING to the repo, never inside it — landed outside that
  `TempDir`'s own cleanup scope on every passing test, not just a failing one. Fixed by nesting the
  repo one level under its own private temp root (`<root>/repo`) in both files, so the sibling
  worktree directory — and the `.git/worktrees/<name>` admin registration `git worktree add` writes
  inside the repo's own `.git` — both stay inside `_root` and are reclaimed by its `Drop`, which runs
  on success, on assertion failure, and through a panicking unwind alike; no git-specific teardown
  call is needed because there is nothing left outside the temp root for one to miss. Proven by
  `real_repo_worktree_fixture_leaves_nothing_in_temp_on_success` and its `_when_the_test_panics`
  counterpart.
- **Test-infra fix found along the way (not reviewer-flagged, surfaced by chasing an intermittent
  local flake):** `compact_hook_dir()` derives from `self.root.parent()`, which for
  `test_registry()`'s disposable tempdir is the SHARED SYSTEM TEMP DIRECTORY — so every test that
  spawns a Claude agent (the common case across the whole suite) was writing/reading the SAME real
  script file, racing every other such test. Fixed with a test-only override
  (`compact_hook_dir_override`/`set_compact_hook_dir_override`, mirroring the existing
  `copilot_agents_dir_override`/`claude_projects_dir` pattern), wired into all four `test_registry`
  helpers. Confirmed via 5 consecutive full-suite runs with no failures, after having reproduced
  the flake reliably enough to trace it.

**rev-4 review round 3 (safety):** Claude's hooks reference confirms `PreCompact` is a BLOCKING
event — exit code 2, or `{"decision": "block", "reason": "..."}`, prevents the compaction from
happening at all. Copilot's own reference documents the identical behavior for its `preCompact`.
A hook script whose marker-write logic can fail in a way that escalates to a nonzero/blocking
exit would mean a bug in loomux's OWN generated script could block a user's compaction outright —
categorically worse than the "no signal, falls back to the pre-#417 inference tier" degrade this
feature is supposed to guarantee on any failure. Auditing both scripts under REAL execution (not
just reasoning about shell semantics) found a genuine bug: the original marker write used a bare
`: > "$path"` redirect, and POSIX makes a redirect that fails to open its target FATAL for a
non-interactive shell — it aborts the whole script before ever reaching the trailing `exit 0`,
and even the `2>/dev/null` on that SAME line never gets applied (the shell fails to set up the
first redirect before it can process the second). This is exactly the shape of failure a full
disk, a permissions problem, or a stale/deleted group directory could trigger — not a contrived
edge case. Fixed by writing the marker with `touch` instead: `touch`'s own failure is an ordinary
COMMAND failure (its own error handling, not the shell's redirection machinery), which a
non-interactive shell reports and continues past normally. Both real-execution tests
(`compact_hook_script_sh_exits_zero_when_the_marker_dir_cant_be_created`,
`copilot_precompact_hook_bash_exits_zero_when_the_marker_dir_cant_be_created`) caught the ORIGINAL
`: >` version failing for real (process exit code 1, not 0) before this fix — genuine
red-before-green, not asserted after the fact. The PowerShell hook command was additionally
hardened with `try`/`catch` and `-ErrorAction Stop` around both `New-Item` calls, since
PowerShell's default error preference makes whether a given cmdlet failure is terminating
somewhat provider-dependent; forcing it to always be terminating and unconditionally swallowing
it in the `catch` removes that ambiguity, pinned by a Windows-only real-execution test
(`copilot_precompact_hook_powershell_exits_zero_when_the_marker_dir_cant_be_created`).

**rev-4 review round 3 (matcher precision):** Claude's `SessionStart` fires with exactly five
`source` values: `startup`, `resume`, `clear`, `compact`, `fork`. loomux's hook config matcher is
the exact string `"compact"` (letters-only matcher syntax, so it's an EXACT match against the
source, not a prefix/regex) — verified to fire on that source alone. The other four are
deliberately NOT wired into this hook, each for a stated reason rather than left unconsidered:
`startup`/`clear` need no re-grounding here at all, since a fresh/cleared session already gets
loomux's own FULL kickoff prompt (not this hook) as its first turn; `resume` is handled by a
SEPARATE, already-shipped mechanism (`resume_kickoff_notice`, #411, above) rather than this hook —
the two were never meant to be the same delivery path, and adding a second, hook-driven channel
for the identical event would be redundant complexity for no clear gain (the same reasoning #411's
own note already gives for not building a native resume hook); `fork` is deliberately left
unwired because a forked session INHERITS its parent's context verbatim (per the docs) rather than
summarizing or reloading it — nothing is diluted by forking, so there is nothing here to
re-ground. Finally, the reference notes `SessionStart`/`Setup` typically fire BEFORE MCP servers
finish connecting — a non-issue for this hook specifically, since its script needs zero MCP
connectivity: it only ever writes a marker file and (for `sessionstart-compact`) prints a fixed,
generic string to stdout.

**rev-4 review round 4 (Copilot `/compact` correction) — another wrong claim, corrected plainly
rather than edited away.** The "Copilot wiring (correction round)" subsection above widened
`compact_nudge_tick`'s admission gate to Copilot but deliberately kept `compact_nudge_cli_
supported` (the narrower "has a `/compact` loomux can paste" gate) claude-only, on the premise
that Copilot has no `/compact` command at all — re-asserted explicitly at the two sites that
actually paste it, and pinned by a NEGATIVE test
(`compact_nudge_tick_never_pastes_slash_compact_into_a_copilot_pane`) proving loomux would never
type it into a Copilot pane. **That premise was wrong.** GitHub's own CLI command reference
([docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference))
documents `/compact [FOCUS-INSTRUCTIONS]`: "Summarize the conversation history to reduce context
window usage. Optionally provide focus instructions to steer the summary." — a real, built-in
Copilot CLI command, identical in spirit (and near-identical in syntax) to Claude's own.

The linked context-management page
([docs.github.com/en/copilot/concepts/agents/copilot-cli/context-management#compaction](https://docs.github.com/en/copilot/concepts/agents/copilot-cli/context-management#compaction))
additionally documents Copilot's AUTOMATIC compaction: it begins in the background at roughly
80% context-window usage, and if compaction hasn't finished by roughly 95%, Copilot pauses and
waits for it rather than proceeding. This confirms the `preCompact` hook's own `trigger:
"manual"|"auto"` field (read in full during the previous correction round) covers BOTH paths —
loomux's trusted-arm handling already treats every fresh marker identically regardless of
`trigger`, since the marker's mere existence (not its parsed payload) is the whole signal, so no
code change was needed for the auto-compaction path specifically — it was already covered, just
not yet documented as confirmed.

**The fix:** `compact_nudge_cli_supported` widens to `matches!(cli, "claude" | "copilot")`. Since
this makes it identical to `compact_hook_cli_supported` for both currently-supported CLIs, the
two gates collapsed back into one function — keeping both as separate stubs with identical
bodies would be exactly the unneeded duplication CLAUDE.md's no-premature-abstraction guidance
warns against. The heuristic/requested-fire sites' own re-check of the (now-removed) narrower
gate is gone too — the loop's single top-of-function admission gate already guarantees `/compact`
paste capability for anything that reaches those sites. `request_compact`'s own CLI-gate error
message no longer hardcodes "Claude-Code-only" (a copilot caller is now accepted; the check
itself is retained as belt-and-braces against a hand-edited or pre-existing group.json with an
unsupported CLI string, since `Guardrails::clamped()` and `spawn_agent`'s own per-role validation
mean no group/agent created through the current API can ever reach it with an unsupported value —
see `request_compact_now_accepts_a_copilot_caller`'s sibling test note in `tests/orchestration.rs`
for why no integration test exercises that branch directly anymore).

**Updated capability matrix for #417** (supersedes the informal claims embedded in the
"Copilot wiring" prose above — that section's ARM/CONFIRM analysis is otherwise unchanged, only
the `/compact`-paste row was wrong):

| | `preCompact`/`PreCompact` hook | `/compact` loomux can paste | native re-grounding on resume |
|---|---|---|---|
| Claude | ✓ (arms trusted) | ✓ | ✓ — `SessionStart(compact)` injects `additionalContext` natively |
| Copilot | ✓ (arms trusted — covers both `trigger: "manual"` and `trigger: "auto"`, i.e. Copilot's own ~80%/~95% automatic compaction too) | ✓ (as of this correction) | ✗ — no compact-sourced `SessionStart` equivalent; reinjection is always via loomux's own delivery, one tick later than Claude's native path |

**Tests flipped red-before-green:** the negative test above inverted to
`compact_nudge_tick_does_paste_slash_compact_into_a_copilot_pane` (proving the paste now fires);
a second, previously-negative test (`compact_nudge_skips_a_cli_with_no_compact_equivalent`) that
made the identical wrong claim for the plain heuristic-fire path inverted to
`compact_nudge_no_longer_skips_copilot_which_has_its_own_compact_equivalent`; and a new full-loop
test (`compact_nudge_tick_copilot_full_loop_paste_then_hook_confirms_then_reinjects`) chains both
of #417's Copilot halves back-to-back for the same agent — the loomux-initiated paste (trusted by
provenance, resolved via busy-then-quiet) followed by an independent `preCompact` hook marker
(trusted by the hook itself, resolved via the delivery-confirmation phase) — proving the
detection/recovery loop closes end to end via either path, exactly as it already did for Claude.
`deliver_prompt` itself needed no change: its `/compact`-paste call site was already routing
through the same CLI-aware-but-content-agnostic delivery machinery (`submit_sequence(cli)` for
the Enter/focus-in sequence, `bracketed_paste` for the text) used for every other prompt this
registry ever sends to any pane — nothing in that path singles out Claude, or ever did.

**#417 correction round 5: slimming the re-grounding notice.** User-directed, after #416/#417
docs verification, not a wrong-claim correction like rounds 2/4 above — the underlying FACTS
this notice's design rested on changed (#416 landed after #328/#329's original "always embed
the full instructions file" decision), so the notice is revisited rather than retracted. Once
the block's full CONTRACT rides the CLI's own system-prompt layer for almost every agent (#416:
Claude's `--agents`, unconditionally, for every block; Copilot's generated `~/.copilot/agents/
*.agent.md` for the default roster and inline `prompt:` blocks), re-embedding that same text
verbatim in the post-compact notice is pure waste — the agent's system prompt already holds it,
permanently, immune to whatever the compaction summarized away.

- **Both CLIs' own docs confirm compaction only touches conversation history, never the system
  prompt.** Claude Code's hooks reference frames `PreCompact`/`SessionStart` entirely in terms
  of summarizing the CONVERSATION; Copilot's context-management page
  (docs.github.com/en/copilot/concepts/agents/copilot-cli/context-management#compaction) is
  more directly on point — its 4-step compaction process explicitly states the summarizer
  preserves "original user instructions" as one of the things a compaction keeps. Between the
  two, there's a solid basis for trusting the system-prompt layer survives structurally; the
  one soft spot (Copilot's docs don't specifically confirm an `agent.md` FILE'S region of the
  prompt architecture, as opposed to instructions given inline) is covered by the demo script
  below, not asserted blind.
- **One flag, decided once, at spawn:** `PersonaInject::contract_on_system_layer` — `true` for
  the Claude branch (unconditionally: the contract always rides `--agents` now) and for a
  Copilot block on the generated-wrapper path (`write_copilot_agent_file` succeeded); `false`
  for a Copilot block resolving to an unambiguous user-authored native `.github/agents/*.md`
  persona (only the user's OWN file rides `--agent` — the documented #416 residual gap, see the
  per-CLI capability matrix above) and for the rare `~/.copilot/agents`-unwritable fallback
  (only the persona text, not the full mechanics-core contract, reaches the kickoff in that
  failure case). Copied onto `AgentEntry.contract_on_system_layer` at spawn and never mutated —
  a persona/workflow-file edit takes effect on the agent's NEXT spawn, same as every other
  `persona_inject` output.
- **`compact_reinjection_notice` picks ONE of exactly two shapes from that single flag** —
  never from which of the six trigger paths (loomux-initiated, agent-requested,
  threshold-escalation, manual `/compact`, the auto-compact banner, or a trusted hook marker)
  detected the compaction, and never from hook-tier vs. inference-tier detection. System-layer
  durability is a LAUNCHER property of this agent, not a property of how loomux happened to
  notice the compaction — so there is exactly one notice per agent, not one per detection path:
  - **Slim (`contract_text: None`, the common case):** never re-embeds the contract. States
    plainly that it already rides the system prompt and survives structurally, then re-syncs
    only what actually ISN'T durable anywhere but a live query: `list_tasks` (task board),
    `get_state` (durable state), `list_agents` (roster). The directive ledger gets BOTH a named
    path pointer AND its tail still inlined verbatim (`directive_ledger_embed`, unchanged,
    same cap) — belt-and-braces, since a directive is qualitatively different from every other
    re-sync target: a tool call can re-derive the task board or durable state on demand, but a
    directive already given can never be re-asked for, so it stays the one thing worth paying
    the extra bytes to inline rather than merely point at.
  - **Verbose (`contract_text: Some(text)`, the one documented exception):** unchanged from
    #328/#329 — the full instructions-file text, read back and embedded verbatim, plus the
    ledger, for the one case that has no system-prompt-layer copy of the contract to trust
    instead.
- **The instructions file is only read back when it's actually going to be used.** The
  reinjection call site now checks `contract_on_system_layer` BEFORE calling
  `fs::read_to_string` on the instructions path at all — the common (slim) case never pays for
  a read whose result it would then throw away.
- **The SessionStart(compact) native `additionalContext` line was ALREADY this slim** — verified,
  not changed. It already named the contract as durable (`--agents`) rather than re-embedding
  it, and already pointed at (never inlined) the ledger path rather than parsing/truncating it
  in shell — deliberately: this one script is generic for the whole machine (constraint #8), and
  duplicating `directive_ledger_embed`'s capped-tail truncation logic in POSIX shell would be
  exactly the two-implementations-drifting-apart risk that function's own doc already argues
  against for the Rust side. So the two channels agree on CONTENT — the contract is durable,
  re-sync via `list_tasks`/`get_state`/`list_agents`, the ledger's location — without being
  byte-identical strings, and whichever one fires for a given agent, that agent gets the same
  information either way.
- **Tests:** the two pre-existing pure-function tests renamed and repointed at the verbose
  branch (`compact_reinjection_notice_embeds_the_instructions_verbatim_when_the_contract_is_
  not_durable`, `compact_reinjection_notice_verbose_folds_in_the_ledger_when_present`); two new
  slim-branch tests
  (`compact_reinjection_notice_is_slim_when_the_contract_rides_the_system_layer`, `..._slim_
  still_inlines_the_ledger_tail`), one of which pins a byte-length ceiling so the notice
  actually reads as slim, not just structurally different; two new integration tests in
  `tests/workflow.rs` (`contract_on_system_layer_is_false_only_for_an_unambiguous_copilot_
  native_persona`, `..._is_true_for_the_generated_copilot_wrapper_and_every_claude_block` —
  **renamed in round 8** to `contract_on_system_layer_is_false_for_every_copilot_block_and_
  true_for_every_claude_block`, since the Copilot half of its own name stopped being true —
  see "Round 8" below) pinning the flag itself against real persona-resolution fixtures, not
  just the notice function in isolation. Red-before-green on both the flag's native-persona
  branch and the notice's slim-selection call site.

**#411, folded in because the plumbing was already open:** the orchestration-RESTORE kickoff (an
app restart resuming a live session) is a fixed string with no directive-ledger embed, unlike the
post-compact reinjection notice — filed separately in #411 during #329's own testing as a
deliberate scope cut. Since `directive_ledger_embed`/`DIRECTIVE_LEDGER_EMBED_CAP_BYTES` needed no
changes to reuse, `resume_kickoff_notice` (mirroring `compact_reinjection_notice`'s exact shape)
folds the SAME ledger embed into the resume-kickoff string — a missing/empty ledger reproduces
the pre-#411 fixed string byte-for-byte, so a group that never calls `note_directive` sees no
change. A SessionStart(resume)-sourced native hook was considered as an alternative delivery for
this and set aside: the resume-kickoff is already a loomux-composed prompt (not relying on
Claude's own context recall the way a mid-session compact does), so a second, hook-driven
delivery channel for the same restart event would be redundant complexity for no clear gain —
noted here rather than built.

### Round 8: two live-demo blockers on the Copilot orchestrator, and a per-CLI composition split

Two live-demo failures, in immediate succession, on a Copilot orchestrator launch — neither a
review finding, both root-caused against GitHub's own docs rather than by inference (the
`agent-cli-reference` skill's discipline).

**8a — `write_copilot_agent_file` never wrote `description:`.** `CustomAgentLoadFailedError: ...
custom agent markdown frontmatter is malformed: description: Required`. GitHub's custom-agents-
configuration reference (docs.github.com/en/copilot/reference/custom-agents-configuration) lists
`description` as required and `name` as optional (defaults to the filename) — the mirror image
of round 6's Claude blocker, on the ORIGINAL round-1 mechanism this whole file-based design
started from, which had evidently never survived a real Copilot launch before this demo. Fixed
with a short, deterministic description built from `group`/`block.id` alone (no timestamp, no
persona text — byte-identical across renders of the same block, matching `write_mcp_config`'s
own re-write-on-every-spawn idempotence). The same doc's CLI how-to
(docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/create-custom-agents-for-cli)
confirmed `--agent <value>` resolves against the FILENAME stem, never the frontmatter `name`
field — the opposite of Claude's own sub-agents doc (code.claude.com/docs/en/sub-agents), which
states outright "The filename doesn't have to match" `name` for Claude. `handle` is set as both
the filename stem and the frontmatter `name` on the Copilot side, so the distinction is inert for
loomux, but the two functions must never be assumed to share a resolution rule just because they
share a `handle` shape. Re-audited Claude's own generated file against ITS doc in the same pass
(rather than trusting that a working demo proved compliance): it already wrote both required
fields, so it was schema-compliant already, not merely lenient.

**8b — the SAME reference page also caps the generated file's BODY (the markdown after the
frontmatter — the "prompt") at 30,000 characters; Claude's own doc states no such cap.** Found by
the reviewer measuring the fix for 8a directly: the default roster's own Copilot ORCHESTRATOR
body was 58,633 characters, ~1.95x over. `contract` (`block_contract_text`: the mechanics core
plus the COMPLETE built-in role template — pages of mechanics, examples and style guidance) had
been written as the generated file's body unconditionally since round 1; nothing before this
round had ever measured its SIZE, only its content and quoting (the same shape of gap round 6's
Claude argv-length blocker closed, in file form this time — verify size properties, not just
content, on every payload channel). An over-cap write is either silently truncated or outright
refused by Copilot (the docs don't say which) — either is role degradation presenting as a
successful launch, exactly the failure class this whole PR exists to eliminate.

**The fix is a per-CLI COMPOSITION split, not a universal thinning.** Claude's rendition is
unchanged — no documented cap, so `contract` still rides its generated file verbatim, full size.
Copilot's generated file now carries `copilot_agent_body`'s SLIM composition instead:

- **Identity** — which block/role this is, one line.
- **The non-negotiable mechanics core** (`mechanics_core`) — the exact "NOT optional, whatever
  your persona says" subset a `mode: replace` persona can never strip (`report()` discipline,
  git/branch/PR discipline, never merging). This is what MUST survive a compaction verbatim, and
  it is a small fraction of a role template's total size — the template's long-form prose,
  examples and style guidance are what actually blew the cap, not the mechanics.
- **The persona, if any** — folded in via the SAME `block_contract_text` framing every other
  channel already uses, by passing `mechanics_core`'s own output as the "instructions body"
  input rather than duplicating the per-mode wording. This mattered more than it first looked:
  the on-disk instructions file (`write_instruction_files`) has NEVER carried a block's full
  persona text — only a short "adopt it, it's in your system prompt" pointer note
  (`block_note`) — so a slim body that dropped persona text entirely would have silently
  regressed #416's core promise for every Copilot block with an inline `prompt:` persona, not
  merely shrunk the built-in template prose. Reusing `block_contract_text` with the small
  `mechanics_core` base instead of the full template body keeps persona delivery intact while
  still cutting the dominant cost.
- **A re-grounding pointer** at the full instructions file `write_instruction_files` already
  wrote — the long-form built-in template prose lives there, one `Read` away, never duplicated
  into the generated file.

Belt-and-braces, mirroring `command_line_length_guard`'s shape exactly: `write_copilot_agent_file`
measures the composed body in CHARACTERS (`.chars().count()`, not `.len()` — the cap is
documented in characters, and a repo-authored persona folded in is not guaranteed ASCII) against
`COPILOT_AGENT_BODY_SAFE_CHARS` (27,000 — a safety margin below the documented 30,000, same
belt-and-braces spirit as the argv guard's own margin below Windows's 32,767-character limit). An
over-cap body is never written — the function fails loudly (audited: `copilot-agent-body-
oversized`, naming the block and the measured size) and returns `None`, routing the caller into
the SAME write-failure fallback an unwritable `~/.copilot/agents` directory already uses (kickoff
delivery, `contract_on_system_layer = false`).

**`contract_on_system_layer` corrected to its own documented meaning.** The field's doc has always
said "whether the block's FULL CONTRACT... actually rides this agent's system-prompt layer" — a
Copilot block on the (pre-round-8) generated-wrapper path set it `true`, which was accurate then
(the file DID carry the full contract) and became FALSE the moment the body slimmed. It is now
`false` for every Copilot block, full stop — the generated-wrapper path joins the native-persona
and unwritable-fallback cases that were already `false`. Consequence, and it is the correct one:
every Copilot compaction now routes through loomux's own VERBOSE reinjection (the full contract
re-embedded from the instructions file — see "#417 correction round 5" above), matching Copilot
having no native post-compact re-grounding channel of its own anyway. Claude is untouched — every
Claude block stays `true`, no documented cap on that side.

**Measured before/after** (default roster, Copilot CLI; `contract` = pre-fix body, `copilot_agent_
body` = post-fix): worker 2,163 chars, reviewer 5,278 chars, planner 1,574 chars, orchestrator
1,587 chars (down from 58,633 — a ~97.3% reduction) — every block comfortably under both the
27,000-char safety margin and the 30,000-char documented cap, with real headroom rather than a
near-miss.

**Tests:** `generated_agent_files_satisfy_each_clis_documented_required_frontmatter_fields` (8a,
reproducing the exact live-incident shape — a default-roster, no-persona Copilot orchestrator —
via a real YAML-frontmatter parse instead of the pre-round-8 `starts_with("---\nname: ...")`
prefix check, which happily passed a file missing `description:` entirely);
`every_default_roster_block_stays_under_copilots_documented_body_cap` and `a_workflow_declared_
copilot_roster_also_stays_under_the_documented_body_cap` (8b, every default-roster block plus a
custom roster with both an inline `prompt:` persona and a `mode: replace` file persona — the
latter reached via the same ambiguous-native-handle shape `copilot_native_agent_is_refused_when_
the_handle_names_a_different_file` uses, since an UNAMBIGUOUS native profile bypasses loomux's own
composition entirely and has no cap concern of loomux's to test);
`copilot_agent_body_over_the_cap_fails_loudly_into_the_write_failure_fallback` (the guard itself:
a ~40KB persona pushes the composed body over the cap, and the test asserts no file is EVER
written at any handle-shaped path, not just that the returned handle is `None`);
`contract_on_system_layer_is_false_for_every_copilot_block_and_true_for_every_claude_block`
(renamed from the pre-round-8 test whose own name stopped being true; renamed AGAIN in the
rev-16 reviewer delta below, to `contract_carrier_is_system_layer_core_for_the_copilot_
generated_wrapper_and_full_for_claude`, once this exact assertion's own premise stopped being
true a second time). Red-before-green: the 8a fix was reverted locally and the
frontmatter-schema test confirmed to fail with the incident's own error text before being
restored.

**Sanity-swept, flagged rather than fixed** (scope discipline — the ask was to check, not chase):
whether the autopilot-consent flow interacts with an active `--agent` handle, and the exact
kickoff-paste timing relative to a `--agent` load failure. Neither has a code path coupling it to
this round's changes, but neither had been exercised through a real Copilot launch before this
demo either — noted for whoever demos those paths next, not built speculatively here.

### Round 8 reviewer delta (rev-16): the lossy bool becomes `ContractCarrier`, plus N3a/N3b

**N2, the substantive finding.** The round-8 B1 fix (above) set the pre-enum bool
`contract_on_system_layer` to `false` for the Copilot generated-wrapper's happy path — accurate
in the narrow sense that the FULL contract no longer rode there, but it collapsed a genuine
THREE-state fact into two. Before round 8, "full contract" (Claude) and "kickoff only" (Copilot's
residual gaps) were the only two states that existed, so the bool was lossless. Round 8's own
slimming introduced a real third state — a durable CORE (identity + `mechanics_core` + a pointer)
that is neither "full" nor "nothing" — and forcing it into the same bucket as genuinely
undurable agents meant EVERY Copilot compaction paid for a full verbose re-embed of the whole
instructions file (tens of KB), right after a compaction meant to reclaim exactly that context,
and directly counter to round 5's own user-directed slimming principle.

Fixed by replacing the bool with `ContractCarrier { SystemLayerFull, SystemLayerCore,
KickoffOnly }`, threaded through `PersonaInject`, `AgentEntry`, and a new `reinject_shape`
(replacing `reinject_contract_text`) that returns a matching `ReinjectShape { Slim, Pointer,
Verbose(String) }` for `compact_reinjection_notice` to render:

- `SystemLayerFull` → `Slim` — nothing to re-embed or point at (Claude, always).
- `SystemLayerCore` → `Pointer` — re-read the full instructions file; never re-embed it (Copilot's
  generated-wrapper happy path, round 8's new third state).
- `KickoffOnly` → `Verbose(text)` — the true fallback, full embed, nothing else is durable
  (Copilot native persona, unwritable directory, or an over-cap body the write guard refused). A
  failed read in this state degrades to `Pointer`, never `Slim` — `Slim` would falsely claim
  durability this agent doesn't have; `Pointer` ("go read the file") stays honest even when the
  read attempt itself failed, and the caller audits that specific degradation
  (`compact-reinjection-contract-unreadable`) since a genuine `SystemLayerCore` agent's `Pointer`
  shape is the CORRECT outcome, not something to flag.

**This also closes N1 structurally, not just for this round.** rev-16 named the staleness of the
`AgentEntry::contract_on_system_layer` doc and the `to_reinject` processing comment as a 3-round
pattern on this PR — both described a binary fact the bool could still technically hold even
after round 8 changed what was actually true. An enum can't drift the same way: there is no
fourth state for a stale comment to silently describe, and every match arm that used to read
`if contract_on_system_layer { .. } else { .. }` is now an exhaustive 3-way match the compiler
enforces.

**Persistence checked, not assumed.** `AgentRecord` (`agents.json`, the one roster structure
actually written to disk) does not carry this fact at all — `AgentEntry` (where it rides at
runtime) derives only `Clone, Debug`, no `Serialize`/`Deserialize`, and is recomputed fresh by
`persona_inject` on every spawn and every resume. So there is no serde migration surface for this
field, and no compat shim was built for one — confirmed by reading the actual persistence code,
not assumed from the field's name.

**N3a.** Both `write_claude_agent_file` and `copilot_agent_body` now append a short,
CLI-agnostic self-check clause (`compaction_self_check_clause`) instructing the agent to re-read
its instructions file after ANY compaction or context loss, independent of whether loomux's own
reinjection notice arrives — cheap insurance (a couple hundred bytes) against a missed or delayed
delivery, never a replacement for the primary channel.

**N3b, widened by rev-18.** Claude's `--append-system-prompt-file` write-failure fallback points
at the instructions file, which is mechanics/template only — a block's actual persona text lives
only in `contract` (the thing that just failed to write), so this specific combination silently
dropped the persona before N3b. Landed narrowly scoped to `mode: replace`; rev-18 pointed out the
scope had no reason to stop there — `render_block_instructions`'s append branch ALSO only ever
writes a short "adopt your persona" pointer note, never the persona's own words, so the gap is
identical in both modes, at no added design cost to covering both. The audit
(`claude-fallback-persona-dropped`) now fires for any non-empty persona on this fallback path,
mode-tagged in its own payload rather than scoped by mode.

### Round 9 (#428): Copilot's own terminal path — a completion-marker accelerator

Live incident, audit-verified: a Copilot orchestrator's compact arm sat at "awaiting evidence
(hook-confirmed)" for 240 seconds — `preCompact` hook evidence landed at +0s, but nothing resolved
it until the user happened to type a question into the pane, triggering busy-then-quiet 60 seconds
before the 300-second `ARM_PENDING_TIMEOUT_MS` would have false-timed-out it instead. On a pane
that stays genuinely idle after compaction (nobody prompts it), that false timeout is not a near
miss, it is deterministic — round 7 closed this exact class for Claude (`SessionStart(compact)` as
an instant terminal path); Copilot never had an equivalent, because GitHub ships it no post-compact
signal of any kind: no `postCompact` event, and `sessionStart` fires only on `startup|resume|new`,
never `compact` (both docs-confirmed in earlier #417 rounds).

**Updated terminal-path asymmetry table** (distinct from the delivery-channel matrix above — this
is about how FAST an arm resolves, not what re-grounding channel it uses once resolved):

| | terminal-path signal | resolution semantics |
|---|---|---|
| Claude | `SessionStart(compact)` hook marker — instant, structural | Marker consumption IS proof re-grounding was delivered (native `additionalContext`) — resolves straight to done, no reinjection needed (round 7) |
| Copilot | its own compaction-completion PAINT ("Compaction completed" / "A new checkpoint has been added to your session.") — an accelerator, not a hook; busy-then-quiet is still the fallback | The paint proves compaction FINISHED, never that re-grounding was delivered (Copilot has no native `additionalContext` channel) — converts the ARM into a DECIDED reinjection immediately, the same action busy-then-quiet's "confirmed" branch already takes, just without waiting for a quiet tick to observe it (round 9) |

**Design constraints, all satisfied:**

- **Accelerator, not replacement.** This is UI text Copilot happens to paint, not a documented API
  — `copilot_compaction_marker_substrings`'s own doc states the fragility explicitly and points at
  the exact strings to re-verify if this stops firing. Busy-then-quiet runs completely
  unconditionally regardless of whether the marker ever matches, so a future Copilot release
  changing this wording degrades back to today's (slower, but correct) resolution — never to a
  hang. Mirrors `auto_compact_banner_substrings`' existing accepted fragility for the RUNNING side.
- **Resolution semantics, correctly asymmetric.** Unlike Claude's SessionStart block, the new
  Copilot block does not skip reinjection — it decides one, reusing the exact "confirmed" logic
  the busy-then-quiet resolver already runs (same instructions-path/ledger-path construction, same
  `to_reinject` push), just triggered by the marker instead of a quiet observation.
- **The rev-10 B1 lesson, re-applied.** Gated on `a.compact_reinject_attempted_ms.is_none()` — if a
  reinjection is already in flight (decided by busy-then-quiet or an earlier marker match for the
  SAME cycle), this block is a genuine no-op: no re-deciding, no touching those fields, the exact
  ordering hazard B1 named for the SessionStart block applies identically here.
- **Provenance (#424/#427).** Gated on `a.compact_pending` (an arm must already be open — a stale
  mention from a long-resolved compaction in scrollback can never resurrect anything; nothing here
  can set `compact_pending` true from `false`) and on `now >= a.compact_inference_guard_until_ms`
  (the same cooldown `human_typed_compact_detected`/`auto_compact_banner_detected` use). Checked,
  not assumed, that loomux never writes either matched sentence into anything it pastes
  (`compact_reinjection_notice`'s three shapes, `compact_escalation_notice`, the bare `/compact`
  command) — grepped those functions' actual bodies before claiming there is no loomux-authored
  echo this could ever match.
- **Fixture provenance.** Both matched sentences are quoted directly from #428's own issue body and
  comment (the user's live screen observation), not reconstructed from memory. A third fragment the
  issue also quotes — "Use /session checkpoints N to view the compaction summary." — is deliberately
  NOT matched: `N` is a checkpoint number that changes every time, so that literal text never
  repeats.

**Tests:** the fast-path proof (`copilot_compaction_marker_resolves_the_arm_even_while_the_pane_
stays_busy` — armed, continuously busy every tick, never quiet, marker still resolves it on the
exact tick it's observed); the B1-shaped no-op (`copilot_compaction_marker_with_no_arm_is_a_no_op`);
the ordering-hazard no-op (`copilot_compaction_marker_while_a_reinjection_is_already_in_flight_is_
a_no_op`); the regression pin (`copilot_busy_then_quiet_still_resolves_when_the_marker_never_
appears`); and a pure-function test for the detector itself. Mutation bar per #424 discipline,
verified: neutralizing `copilot_compaction_marker_detected` to always return `false` reds exactly
two tests — the pure-function test and the fast-path integration test — and no others, confirming
the fallback path is genuinely independent of the new detector.

Closes #428.

### Round 10 (#428 follow-up, user-directed polish): the evidence-poll cadence, and badge honesty

Round 9's marker fix worked — a user re-test's own audit showed `compact-resolved-copilot-marker`
firing on a genuinely idle pane — but the residual UX was the EVIDENCE POLL CADENCE, not the state
machine: `start_compact_nudge`'s loop only ever woke on `IDLE_TICK_INTERVAL` (60s, shared by
convention with `start_idle_tick`'s own loop, though the two are structurally independent threads),
so a hook marker or the Copilot completion-paint sat unconsumed for up to that whole window even
though the eventual outcome was already effectively decided. The user read the resulting limbo as
"still unfixed."

**Adaptive poll cadence.** `compact_nudge_poll_interval(any_pending: bool) -> Duration` is the pure
decision `start_compact_nudge`'s loop now makes every iteration in place of the fixed constant:
`COMPACT_NUDGE_FAST_POLL_INTERVAL` (10s) while `OrchRegistry::any_compact_pending()` is true
anywhere in the registry, `IDLE_TICK_INTERVAL` (60s, unchanged) otherwise. `any_compact_pending`
is a fresh registry-wide scan (`self.agents.lock_safe().values().any(|a| a.compact_pending)`) —
deliberately whole-registry rather than per-group, since one thread serves every group and the
fast cadence should be "on" if it would help ANY of them. No hysteresis or latch: the interval is
recomputed from scratch every loop iteration with no memory of a previous tick, which is also the
answer to "won't a rapidly opening/closing arm thrash the timer?" — recomputing fresh every
iteration cannot oscillate faster than the loop itself already runs, so there is no additional
state to get wrong. Idle cost is zero: the fast branch is only ever taken while a real compaction
cycle is genuinely in flight, which is a small fraction of a session's wall-clock time.

**The fast-poll scope is registry-wide, and that breadth is deliberate** (rev-25 review, named
explicitly rather than left implicit): one pending arm ANYWHERE in the registry upgrades the poll
cadence for EVERY agent everywhere, not just the one that's actually waiting — `agent_compact_
signals` (what runs more often at the faster cadence) reads every agent's full pty tail, capped at
`OUTPUT_RING_CAP` = 256 KiB, and ANSI-strips it, gated only on the agent having a live pty, not on
being pending or otherwise eligible. Measured bound: ≤ 256 KiB × 6 wakes/min ≈ 1.5 MiB/min per
agent at the fast cadence — sub-1 MiB/s in aggregate even across a large fleet (tens of agents) —
and the window this can run for at all is bounded by the state machine itself
(`ARM_PENDING_TIMEOUT_MS`, 5 min, or up to `MAX_REINJECT_ATTEMPTS` × `REINJECT_CONFIRM_TIMEOUT_MS`,
15 min, if a resolved arm's retries all stall) — under ~20 minutes worst case, never unbounded, and
in the normal case seconds rather than minutes since most arms resolve on the very next fast wake.
Two cheap options if this bound ever needs tightening in practice, neither built speculatively
here: scope the fast cadence per-group instead of registry-wide, or have `agent_compact_signals`
skip the tail read for an agent that is neither pending nor otherwise eligible for compact-nudge's
inference detectors.

**Badge honesty.** `awaiting_evidence` with `source: "hook"` is the phase a hook-sourced arm lands
in IMMEDIATELY (it sets `compact_seen_busy` at arm time — see `compaction_status`'s doc — so it
skips "armed" entirely on the very first tick) and is exactly the phase the live re-test sat in.
The label read as stuck ("compact awaiting evidence (hook-confirmed)") even though the outcome was
already decided: a hook told loomux directly that compaction happened, and resolution was only
ever a matter of the next poll consuming the marker. `compactionstatus.ts` now special-cases this
one state: `"compact confirmed — finalizing"`, tooltip "a hook confirmed this compaction directly
— wrapping up the re-grounding handoff now". The three other `awaiting_evidence` variants (no
source, trusted; no source, unconfirmed) are genuinely still waiting on an outcome busy-then-quiet
hasn't resolved either way yet, so their wording is untouched — this is a targeted relabeling of
one state that stopped being honest, not a vocabulary overhaul.

**No state-machine decision changed** — the 12th consecutive round on this PR to make that claim,
and it holds: every `compact_pending`/`compact_reinject_*`/`ContractCarrier` transition, every
arm/confirm/timeout/abandon condition, is byte-identical to round 9. Only how OFTEN the loop reads
the outcome, and how the human-facing label narrates one already-decided phase, changed.

**Tests:** `compact_nudge_poll_interval_is_fast_only_while_something_is_pending` (the cadence
decision, both directions); `any_compact_pending_is_true_only_while_an_arm_is_actually_open` (proven
against a real registry through the whole arm-through-confirmation lifecycle, not just the pure
function); the frontend label/tooltip test split so the hook-sourced case's new wording and the
three unchanged variants are each pinned separately.

### Min-context floor (benchtest finding, landed alongside #332 — same token-economy theme)

A `loomux-testbed` benchtest of the #398/#332 PR surfaced a second waste pattern on the
compact-nudge side: a single-feature session ran 3-4 real compactions, and every one of them —
correlated against the nearby `compact-pending-discarded` token readings, the closest available
evidence — landed at roughly 20-31% of the context window. The lull-timer's quiet-window gate
was doing exactly its job (firing at a genuinely idle moment); the missing piece was any
awareness of *how full the pane actually was* before paying a full re-grounding cycle for it —
right timing, wrong context level.

- **The floor gates the HEURISTIC fire only.** `Guardrails.compact_nudge_min_context_percent` is
  checked by `compact_nudge_context_floor_met` and ANDed into `heuristic_fires` alongside the
  existing role/threshold checks in `compact_nudge_tick`. It is never applied to `requested_fires`
  (`request_compact`) — an agent that explicitly asks is always honored regardless of context%,
  exactly per #328's original framing: loomux's own unprompted judgment is what gets a floor, not
  the agent's.
- **Smart default (rev-65 review round): tri-state `Option<u32>`, resolved at the GATE, not at
  `clamped()`.** A first cut shipped this as a plain `u32` with "0 = off" — but that shape asked
  the human to configure a percentage just to get a fix for a problem they didn't cause, and the
  reviewer flagged that a re-benchtest at the (then) default config would reproduce the exact
  over-compaction the finding was about. The fix: `None` (unset — what every group gets with zero
  config) resolves to `DEFAULT_COMPACT_NUDGE_MIN_CONTEXT_PERCENT` (50) automatically **whenever
  `compact_nudge_minutes > 0`** — enabling the quiet-window alone is enough, no second field to
  touch. `Some(0)` is the explicit opt-out (today's pre-smart-default behavior, preserved
  verbatim), `Some(n)` an explicit floor. The resolution deliberately lives in
  `compact_nudge_context_floor_met` (reading the LIVE `compact_nudge_minutes` every tick already
  has in hand) rather than in `Guardrails::clamped()` — a group that flips `compact_nudge_minutes`
  on later via a live setter gets the smart default immediately, with no re-launch or
  re-normalization pass needed to "catch up". `Option<u32>` mirrors the one other place this
  struct already needed "let a real value distinguish from absence" —
  `context_window_tokens_override`.
- **Fails open with no reading**, in every one of the three states — a missing/stale
  context-percent reading must never silently disable the whole heuristic nudge, the same
  "degrade, don't deny" posture #332's intake gate takes on a `gh` failure.
- **Template guidance, not a tool change.** `orchestrator.md`'s existing "Compact at lulls"
  section and `docs/orchestration.md`'s user-facing **Compact-nudge** section both name the smart
  default and its number (50%) explicitly, so the template and the config never quote different
  figures. The tool itself (`request_compact`) stays unconditionally available at any context
  level, per the design above.
- **Config surface:** persisted in group.json (`null`/absent for `None`, an integer for `Some`),
  live-settable via `orch_set_compact_nudge_min_context_percent` — which always writes an explicit
  `Some` and can never restore `None`, since touching the control at all is the explicit choice
  (mirrors `orch_set_compact_context_threshold`'s shape otherwise).
- **Interaction with the escalation threshold (`compact_context_threshold_percent`) is unchanged
  by the smart default.** The two are independent knobs answering different questions — the floor
  gates the TIME-based lull fire ("don't act on the timer alone below N%"), the threshold drives a
  SEPARATE context-based escalation path ("act because you crossed N%") — and neither reads the
  other's value. A group with both configured can end up with the escalation threshold LOWER than
  the min-context floor (escalation fires before the lull-timer's floor would ever have let it
  through); that is intentional, not a bug to reconcile — escalation is not the lull-timer, and
  crossing its own threshold is authorization enough for a request_compact on the agent's behalf.

## Enforced merge gate (#83)

Template guidance is not a security boundary. A live incident proved it: an orchestrator merged
four PRs straight to `main`, ignoring the "never merge" instruction. So the human merge gate is
now **structurally enforced** — an agent that tries to merge onto the default branch without
consent is *blocked*, not advised.

- **The interceptor.** Every *agent* pane (orchestrator/worker/reviewer/planner) is spawned with
  a loomux `gh` shim prepended to its `PATH` and `LOOMUX_GROUP_DIR` set to its group's state dir.
  The shim (`ensure_gh_shim`, written once under `<data>/loomux/ghshim`) is a POSIX `gh` script
  (plus a Windows `gh.cmd` that delegates to it) with the *real* gh's absolute path baked in, so
  it never re-resolves to itself. Injection is per-pane via a new `SpawnRequest.env` →
  `spawn_pty(env)` → `apply_extra_env` path, so **only agent panes** carry it — a human's own
  shell (in loomux or out) has an untouched `PATH` and pays zero shim overhead. On Windows the
  shim dir is first on `PATH`, and the agent's Bash tool (Git Bash, where Claude Code runs `gh`)
  resolves the extension-less `gh` script ahead of the real `gh.exe`.
- **The shim's own toolchain (#509).** `sh` is launched by absolute path (#335) but still
  inherits the *caller's* `PATH`, which off a PowerShell/cmd pane carries no Git for Windows
  coreutils — so every `tr` the shim normalizes with failed "command not found", its command
  substitution went **empty**, and the whole `gh api` release/merge arm matched nothing and fell
  **open**. The shims now bake in the coreutils directory derived from their own `sh` install
  (`winpath::resolve_utils_dir`, MSYS `/c/…` form) and **assert** every dependency resolves
  before any gate logic runs, refusing loudly (`gate-degraded-missing-dep`) rather than
  half-normalizing. The same PATH gap also broke `gh pr create` outright — a different cause
  (cmd.exe re-parsing a `.cmd`'s arguments), fixed separately. See
  `doc/design/shim-path-integrity.md` for the full audit, the fail-open/fail-closed table, and
  the residual that fix leaves.
- **The decision** is the pure, unit-tested `gh_gate_decision` (the shim mirrors it in shell,
  and a shell harness executes the real script against a fake gh to prove parity): only
  `gh pr merge` (and cheap `gh api` merge shapes — `gh_is_merge_invocation`) is gated. Detection
  parses gh's argv into positionals (`gh_positionals`), skipping the global `-R/--repo <value>`
  and other flags that gh accepts **before or between** the command tokens — so
  `gh -R o/r pr merge` and `gh pr -R o/r merge` are gated, not just the bare form (the rev-79 F1
  hole). The shim asks the *real* gh for the PR's `baseRefName` and the repo's `defaultBranchRef`,
  **honoring the same `-R/--repo`** the caller passed (`gh_repo_flag`) so both resolve for the
  right repo, not the cwd repo (rev-79 F2). A base
  **≠ default** passes through untouched (the integration-branch flow agents rely on); a base
  **= default** is allowed **only** when both the `autonomous` and `auto_merge` markers are
  present; an **undeterminable** base fails safe (block). Every refusal/allow is appended to the
  group's `audit.jsonl` in the backend's own line format (`actor: "gh-shim"`), and refusals exit
  non-zero with a clear message telling the agent to report to the human.
- **The dependency.** Auto-merge authority exists *only* in autonomous mode, enforced at the API,
  not just the UI: `set_auto_merge(true)` is **rejected** unless autonomous is on; turning
  autonomous **off force-disables** auto-merge (audited); a **budget suspension** does the same
  (rev-79 F4); and a stale on-disk `auto_merge`-without-`autonomous` combo (older group,
  hand-edited state) is **reconciled off on read** (audited). The force-disable drops auto-merge
  from the in-memory gate set **unconditionally**, even if the durable marker removal fails (the
  #149 money-stop pattern — in-memory authoritative). So the gate's "both markers present" test
  can never be satisfied by an orphaned `auto_merge` marker. The UI mirrors this (`approvalControl`): with autonomous off the "Require human
  approval" checkbox is locked checked with a tooltip.

### Human-granted one-time exception (grants)

The blanket markers are all-or-nothing, so a human clicking board **Approve** — or saying
"merge it" — was *still* blocked (Approve doesn't set the markers). The fix is a per-target,
one-time **grant** the shim also honors.

- **Grant files.** A grant is a small file under the group dir the shim consults:
  `merge_grants/pr-<N>` (a default-branch merge of PR N) or `release_grants/<tag>` (the release
  of tag `<tag>`). Line 1 is a unix-seconds **expiry** — `GRANT_TTL_SECS` = 30 min for merges,
  `RELEASE_GRANT_TTL_SECS` = 90 min for releases (why they differ: *Two kinds of grant* below);
  the shim treats the grant as valid iff the file exists and now < expiry. Files are written
  with `atomic_write` (temp + rename, temp name = pid + `GRANT_SEQ`, no getrandom) so the shim
  can never read a half-written grant.
- **Two kinds of grant, and the reason they differ (#438).** A **merge** grant is one-time: a
  merge is one action, and "merge PR #7" cannot mean anything else. A **release** grant is a
  **pipeline** grant: "release vX.Y.Z" is not one command but a sequence — push the `vX.Y.Z`
  tag, let `release.yml` build and create the release, write that release's notes — and
  charging the human a fresh authorization per step is a bug, not a safety property. It shipped
  as one: v1.0.0 cost three human touches for one authorized release, and v1.1.0-beta6 spent
  its grant on the tag push and then refused the notes write that belonged to the same release.
  So a release grant is **checked and never consumed** (`RELEASE_GRANT_VALID_SH`, substituted
  byte-identically into both shims from one Rust const). What bounds it instead:
  **tag identity** — the file is keyed by tag segment, so it authorizes exactly one tag's
  release and nothing else; **the TTL** — re-read and re-compared to the clock on *every* step,
  so the window is a hard wall rather than a first-use wall, and an expired grant is deleted on
  sight; and **default-deny** — no file, an unparseable expiry, or a clock the shim cannot read
  at all all refuse. That last one is deliberately stricter than the merge path's claim (which
  treats an unreadable `date` as "not yet expired"): a token spent once can tolerate that, a
  grant that stays live until a timestamp cannot. The bump-PR merge is **not** in scope — it
  stays under the merge gate with its own Approve, so nothing here weakens what a merge needs.
  90 minutes is chosen to cover a slow four-platform matrix plus a re-run of one failed leg;
  #438's other idea — auto-extending the window while steps keep succeeding — was rejected
  because a window any authorized step renews has no bound, and "the agent kept working" is not
  evidence the human still consents.
- **Claim, then settle on the real outcome (#256).** The **merge** grant is not spent on
  interception — it is **claimed** (`loomux_grant_claim`: an atomic `mv` to a `.claimed`
  sibling, so a concurrent claimant loses the race outright rather than double-spending) and
  only **settled** (`loomux_grant_settle`) once the real `gh` call it authorizes has actually
  run: consumed (`rm`) on exit 0, restored to the original path on any other exit so a retry
  can still use it. A merge GitHub itself refuses (draft PR, branch protection, a stale head, a
  transient API error) must not burn the human's one-time grant — the #256 live incident (PR
  #226) hinged on exactly this. If the shim process dies between claim and settle, the original
  grant file stays gone and the orphaned `.claimed` file is never consulted again — a crash
  requires a fresh grant, never a second use. Release grants shared this machinery until #438
  (#303 and #315 were the same bug class for `gh release` and `git push` respectively) and no
  longer need it: nothing is spent, so nothing can be burned by a step that failed, and both
  issues' property now holds by construction rather than by remembering to call settle. The
  git shim, which gates only releases, therefore carries no claim/settle at all.
- **Decision.** `gh_gate_decision` gains a `grant_valid` input: a default-branch merge is
  allowed by `(autonomous && auto_merge)` **OR** a valid grant for *that* PR (`AllowGrant`,
  consumed). The shim resolves the PR **number** via the real gh (`--json baseRefName,number`)
  so a grant for #5 can't authorize merging #7 whatever selector form was used.
- **Approve-with-comment.** The grant-writing methods (`grant_merge` / `grant_release`) take an
  optional comment delivered to the orchestrator with the authorization via
  `deliver_to_orchestrator` — "approved — also bump the changelog first". Board **Approve**
  (`approve_task`) now writes the merge grant for the task's PR and delivers the comment.
- **Bulk approve: delivery, not authority (#507).** Approving a whole board selection
  (`orch_approve_tasks` → `approve_tasks`) mints **one ordinary grant per PR** — the same
  single-use, 30-min `merge_grants/pr-<N>` file, written by the same code — and then says so
  **once**. There is deliberately no bulk grant object: nothing the shim reads changed, so a
  batch cannot widen what any single grant authorizes, and the shim's claim/settle mechanics
  keep working per PR. The split that makes this possible is `mint_merge_grant` (write +
  audit, no delivery) under `grant_merge` (mint + deliver); the consolidated wording comes
  from one pure builder, `merge_grant_notice`, which BOTH paths use — so a single Approve's
  notice is byte-identical to a bulk-of-one's by construction rather than by two strings
  agreeing today. Items approved with no resolvable PR are named in their own sentence, not
  folded into the granted list.
  Two behaviors differ from the sibling batch, `delete_tasks`, on purpose: the batch is
  **all-or-nothing** and duplicates are **errors**. A pre-flight pass over one board
  snapshot checks every id exists and sits at the merge gate, and refuses a repeated task
  id *or* a repeated resolved **PR number** — two rows naming the same PR (a duplicate
  filing) would mint `pr-<N>` twice, the second overwriting the first, and then announce
  "#7, #7 … one grant per PR": two grants claimed, one file on disk. Skipping ids that
  vanished under the human's selection is right for a cleanup — deleting a gone row is a
  no-op — but an authority action that silently grants 4 of 5, with no signal which was
  dropped, is a decision the human did not make. A clean refusal lets them re-tick and
  click again.
  That same pre-flight is where each item's PR number is **resolved**, which is what makes
  the granted-vs-plain split a property of the refs the human selected rather than of
  whether a write happened to succeed. A `mint_merge_grant` failure has two causes —
  no PR number in the ref (the intended plain path) and an `atomic_write` I/O failure (a
  full disk) — and collapsing them with `.ok()` made the second announce *"no PR number
  could be resolved"*: false, and it sends the orchestrator to close out by hand a PR whose
  merge the shim will then refuse. A write failure now propagates and fails the call, so
  some items may be flipped `done` with some grants minted but **nothing announced** — an
  unannounced grant simply expires unused, whereas a confident notice misdescribing what
  was authorized does not un-say itself. `approve_task` carried the same swallow on
  `grant_merge` and got the same fix. (#507 review B1.)
- **Agent-unreachable boundary.** Grants are written ONLY by Tauri commands (board Approve,
  `orch_grant_merge`, `orch_grant_release`) — human surfaces. **No MCP tool** writes them
  (regression-tested: no agent-visible tool name contains "grant", and the file-writing MCP
  tools `set_state`/`upsert_task`/`save_attachment` write only their own fixed paths, never a
  grant path). Agents *consume* grants (the shim) but never *mint* them through loomux.

### The workflow merge gate composes on top (#222 / #197)

Everything above is the **human** gate. A repo that declares `gates.merge` in its
`.loomux/workflow.yml` adds a **second, independent** necessary condition to the same shim:
`gh pr merge` is refused until every reviewer block the gate names has recorded a `pass` via the
`review_verdict` MCP tool (`threshold: N` needs N of them; a `fail`/`escalate` from any of them
refuses outright). It is evaluated **before** every opening above — a grant, `autonomous +
auto_merge` and supervised dangerous mode all sit below it and none of them can satisfy it, which
is what makes #197 Scope B ("an auto-merge must be structurally impossible until every required
review verdict is recorded PASS") true rather than aspirational (executed in the shell harness,
not merely asserted about the source order). A verdict is bound to the PR's **head commit**, so a
pass does not survive a re-push — otherwise the gate reads green over commits nobody reviewed.
Unlike the human gate it applies to non-default merges too, and a refused merge does not consume a
pending grant. Verdicts live in `verdicts/pr-<N>/<block>` and the declared gate in `merge_gate`,
both under the group dir, both in the same small-file shape the shim already reads.

Two notes that belong next to *Honest bypass surface* above. (1) A **merge with no
`LOOMUX_GROUP_DIR` is now refused outright** — an agent pane always has it, so an unset variable
at the shim is evasion; previously it slipped a non-default merge past the workflow gate with
nothing in the audit. (2) The verdict store is forgeable by an agent with a shell exactly as grant
files are, and — unlike the human gate — **a machine account does not close it**: the forge cannot
tell a fabricated verdict file from a real one. Full design + the honest limits:
`doc/design/workflows.md`.

### Release & tag gating

Releases publish to the world — a `v*` tag push triggers `release.yml` (GitHub release + npm),
and `gh release create` does likewise — a strictly bigger blast radius than a merge. So they get
enforcement **parallel to merges but on a SEPARATE, independent toggle**: a release/tag is allowed
when **`(autonomous && auto_release)`** OR by an explicit per-tag grant (`release_gate_decision`,
exactly mirroring `gh_gate_decision`'s `(autonomous && auto_merge) || grant`). `auto_release`
defaults **OFF** and is independent of `auto_merge` — the human can allow auto-merge while keeping
releases manual, opt into both, or neither. (This supersedes the earlier "releases are never
blanket-allowed by autonomous" policy, which conflated "autonomous" with "auto-merge"; the human
live-tested it and asked for hands-off releasing as an explicit opt-in.) Because the default is
off, turning autonomous on never surprise-publishes — releasing stays a deliberate act (the toggle
or a grant). `auto_release` mirrors `auto_merge`'s machinery exactly: gated behind autonomous
(rejects enable when off), disk-first fail-loud disable, force-disabled on autonomous-off / budget
suspension (the money-stop drops it from the in-memory gate set unconditionally), stale-marker
reconcile on read, mirrored into the kickoff config + a live notice, and surfaced additively on
`orch_autonomy` (`auto_release: bool`) via `orch_set_auto_release`.

- **gh shim** additionally gates `gh release create|edit|delete <tag>` (read-only
  `view`/`list`/`download` pass through) — `gh_release_action`. It **also** gates the raw
  **`gh api` / graphql** shapes that publish the same way — otherwise an agent could drive a
  release straight through the API and skip the subcommand gate entirely (**#196**: the release
  path had *less* api coverage than the merge path, which already fail-safe-blocks raw `gh api`
  merges). All of them route through **one** shared shell decision (`loomux_release_gate`, the
  single decision point — no parallel checker), so the api path can never diverge from the
  subcommand path: (a) a **write** (POST/PATCH/DELETE, not GET) to the **`git/refs` / `git/tags`**
  plumbing that creates/moves/deletes a **`refs/tags/*`** ref, (b) a **write** to the `…/releases`
  endpoint (create/edit/delete — read-only GET list/view passes), and (c) a graphql
  **create/update/deleteRelease** mutation *or* a **`createRef`/`updateRef`** of a `refs/tags` ref
  (a `*Tag` mutation). **Decision is by LOCUS, never substring-anywhere** — the shim parses gh
  api's own flags and looks *only* at the request **method** (`-X`/`--method`, else POST when a
  field/`--input` is present, else GET), the **URL path** (query string stripped), and the parsed
  **`ref`/`query` field**. This was the crux of the #196 re-reviews: an argv-substring check gated
  by "is `refs/tags/` anywhere / is `refs/heads/` anywhere", so a **decoy** `refs/heads/` token in a
  `-q` jq filter, a `-H` header, a `-f sha=`, a `?d=` URL query, or an extra field flipped the
  branch exemption while `ref=refs/tags/v9` created the tag; and an **opaque** graphql body
  (`--input`/`-F query=@file`/stdin) hid the mutation entirely. Now the branch exemption fires
  **only** when the ref *locus* is provably heads — the URL path is `…/git/refs/heads/…` **or** the
  parsed `ref` field (argv `-f ref=`, or the `"ref"` read from a readable `--input <file>` body) is
  `refs/heads/…` — **and** `refs/tags/` is absent from that locus. A non-GET write to `git/refs`/
  `git/tags` whose locus can't be proven heads (a `--input -` stdin body, an opaque graphql query)
  **fails safe to the gate** (blanket-markers-only). The tag is resolved for grant-keying from the
  locus (argv `ref=refs/tags/<t>`, a `--input` file's `"ref"`, the URL `…/git/refs/tags/<t>`,
  `tag_name=<t>`, or an inline graphql `tagName:"<t>"` / `name:"refs/tags/<t>"`); where it isn't
  (stdin body, opaque graphql), only the blanket markers (`autonomous && auto_release`, or
  supervised `dangerous && !autonomous`) can allow it — otherwise **fail-safe block**.
  **Release-id addressing resolves (#437).** A call that names a release by numeric id —
  `gh api -X PATCH repos/O/R/releases/<id> -F body=@notes.md`, which is how the release skill
  *mandates* notes be applied, by canonical id rather than by tag lookup that a duplicate
  release can make ambiguous (#282) — carries no tag anywhere in its argv, so it used to be
  unmatchable against any grant and was refused outright (the refusal read `release/tag ()`,
  the empty parens being the whole diagnosis). The gate now resolves the id to its tag with
  **one read-only GET** against the real gh, after the blanket openings so a marker-allowed
  release pays nothing for it, and then gates on that tag like any other.

  **The id is the locus, and it outranks the argv.** This is the part that carries the
  security argument, and the first cut got it wrong: resolution was conditional on the argv
  naming no tag, so `-f tag_name=<granted>` suppressed it and a live grant reached *every*
  release in the repo — `gh api -X PATCH …/releases/777 -f tag_name=v1.2.3 -f
  make_latest=true` keyed the gate on the granted tag, retagged an unauthorized release and
  took `latest`; `-X DELETE … -f tag_name=v1.2.3` deleted one (the API ignores `tag_name` on a
  DELETE, so it was there purely to satisfy the gate). It is the exact mirror of the `#196`
  decoys — those *loosen* the gate with a cosmetic `refs/heads`, this one *satisfies* it with
  a cosmetic tag — and the suite had no case for that direction. So: whenever the URL names
  one specific release, the argv tag is **discarded** before anything is keyed on it, and
  three outcomes follow. Resolved and consistent → gate on the resolved tag. Resolved but the
  argv named a *different* tag → **refuse**, because that is either a retag (publishing a tag
  nobody granted) or a decoy, and letting the resolved tag simply win would allow the retag
  whenever the resolved tag happened to be the granted one. Not resolvable → **refuse**: a
  lookup that errors, 404s, prints nothing, prints `null`, or prints anything that is not a
  plausible ref name leaves the tag empty, and an unresolvable id is never "probably fine".
  `POST …/releases` (create) has no id in the URL, so `tag_name` remains its locus.

  **What makes the traversal case safe is the resolved path, not a pattern.** The GET uses
  `$a_path` — the write's own path, verbatim, not lowercased and not a reconstructed prefix —
  so gh normalizes one string and the two calls cannot address different releases. (An earlier
  cut resolved `${prefix}releases/${id}`, and *there* `…/releases/555/../444` really would
  have read release 555's tag and written to 444.) The `..` and last-segment tests that
  remain are not what closes that; they decide whether the URL counts as **id-addressed**,
  which matters because a release loomux cannot pin down (`…/releases/555/assets`,
  `…/releases/../777`) must still deny the argv tag the chance to speak for it. Each is
  pinned by a mutation that flips a refusal to an allow when it is removed. A non-release api call (an issues endpoint, a branch `refs/heads` write, an
  read-only GET) passes through untouched. The **graphql arm**: the endpoint is recognized by
  **suffix** (`graphql` | `/graphql` | `*/graphql`, incl. the full-URL host form) — not an exact
  `graphql` string, which a `gh api /graphql`/full-URL POST would have slipped (#196 r4) — and it
  gates **every ref/tag/release create+move+delete mutation** (`createRef` | `updateRef` |
  `deleteRef` | `createTag` | `deleteTag` | `create`/`update`/`deleteRelease`) **unconditionally**,
  plus opaque graphql (`--input`/stdin/`@file`). This matches the REST arm's full coverage — POST/
  PATCH/**DELETE** of `git/refs`/`git/tags` and create/edit/**delete** of releases — so a destructive
  `deleteRef` (which can drop a published `v*` tag ref) gates like `DELETE …/git/refs/tags/*`. There is **no "prove a mutation safe from the query text" logic** in the graphql arm,
  by design: every text heuristic tried was defeated by the next encoding — a `refs/tags` literal,
  a `-F ref=` variable, a no-`$`-variables rule — because graphql **variables, comments, aliases,
  and string escapes** (`refs\/tags\/`) each dodge a text scan and the next encoding would too
  (#196 r6). Closing the class (unconditional gate) removes the thing being decoyed. A graphql
  `createRef` targeting a *branch* is a rare corner — agents create branches via `git push` or REST
  `git/refs`, and the **REST arm still passes branch creation by real URL locus** (rev-68 confirmed
  it airtight) — so gating the graphql-branch case fails safe: markers/grant still allow it. A
  non-mutation graphql **read** query carries none of those tokens → passes.
- **git shim** (new, same PATH-injection as the gh shim) gates `git push` that publishes a tag:
  `--tags`/`--follow-tags`/`--mirror` (bulk → blocked, push the specific approved tag),
  `refs/tags/<t>` and the `tag <t>` form (explicit), and a bare **`v*`** refspec (any v-prefixed
  ref) **confirmed a tag** against the real git (`git_tag_push`). The `v*` pattern **must track
  `.github/workflows/release.yml`'s `on.push.tags`** (both `git_tag_push` and the shim carry a
  comment saying so): they matched `v<digit>` at first, which let `vbeta`/`vRelease` publish yet
  slip the gate (rev-86). Local `git tag` is harmless — only the **push** reaches the world — and
  a plain branch push (or a non-`v*` ref like `nightly`, which release.yml ignores) execs the
  real git with **zero** extra work. The gh scanner's value-flag skip list is complete for
  `gh release create` (`--title`/`--notes`/`--target`/… consume their value) so a granted release
  with `--title "…"` before the tag isn't misparsed and wrongly blocked.

### Supervised dangerous mode

The human asked to "just instruct my agent to do merges and releases when I'm here supervising" —
without turning on *autonomous* (which is the hands-off, unattended mode). So a `dangerous_mode`
marker adds a fourth path to the **same single decision point**: a default-branch merge or a
release/tag is allowed when `(autonomous && auto_merge/auto_release)` OR **`(dangerous && !autonomous)`**
OR a valid grant. No new parallel checks — `gh_gate_decision` / `release_gate_decision` gain a
`dangerous` param, and the shims read the `dangerous_mode` marker alongside the others.

- **Mutually exclusive with autonomous**, enforced both ways: `set_dangerous_mode(true)` is
  rejected while autonomous is on; enabling autonomous force-clears `dangerous_mode` (audited +
  a human-visible notice). The shim's `dangerous && !autonomous` guard is defensive — if both
  markers somehow coexist (hand-edit), autonomous wins and the stale `dangerous_mode` is
  reconciled off on read. Unlike `auto_merge`/`auto_release`, `dangerous_mode` is valid
  *standalone* (it IS the not-autonomous mode), so it survives restart without an `autonomous`
  marker.
- **Distinct audit path.** A merge/release allowed via dangerous mode audits `merge-gate-dangerous`
  / `release-gate-dangerous` (vs `-allowed` for the autonomous blanket and `-granted` for a grant),
  so the trail always records *which* gate path let it through — plus `GhGate::AllowDangerous` in
  the pure spec.
- **Agent-unreachable, same as grants.** The setter is a Tauri command
  (`orch_set_dangerous_mode`); no MCP tool can enable it (regression-tested: no agent-visible tool
  name contains "dangerous", and the file-writing MCP tools never create the marker). Surfaced
  additively on `orch_autonomy` (`dangerous_mode: bool`) and mirrored into the kickoff config + a
  live notice. Disk-first fail-loud disable. The marker's FS-forgeability is the same documented
  bypass class as grant files (an agent's shell can `echo` it — closed only by a machine account).
- **No TTL yet — hands-off only.** Dangerous mode is a standing marker with no auto-expiry: the
  human turns it off (or it clears when they enable autonomous). A **TTL / auto-expire** (e.g.
  clear after N minutes of no human input, or a fixed window) is a reasonable future hardening —
  it would bound a forgotten "on" — but was left out per the frozen direction; noted here and in
  the PR for the human to decide.

- **Honest bypass surface** (documented, not hidden). The shims raise the cost of an unattended
  bad merge/publish from "type one command" to "deliberately evade a named control," but a
  determined agent with shell access *can* still: (1) call gh/git by its **absolute path**,
  skipping `PATH`; (2) use a **raw REST/GraphQL** merge/release shape the cheap match doesn't
  catch (unusual `gh api`, a direct `curl` with a token, a library) — or a git remote helper /
  bare `v*` tag push under `-C otherdir` the confirm can't see; (3) run on a machine with **no
  `sh` anywhere at all** — not merely off `PATH` (`#335` closed that broader hole: the `.cmd`
  delegator bakes in an absolute `sh.exe` path resolved from `git.exe`'s own install layout at
  shim-write time, so a PowerShell/cmd pane on a normal Git-for-Windows install routes through
  the gate exactly like Bash does; only a machine genuinely missing `sh` falls through to the
  real binary, and that fallback is itself audited, `gate-degraded-no-sh`, never silent); (4)
  define a **`gh`/`git` alias** re-invoking the real binary; (5) **forge a grant file** directly
  (`echo … > $LOOMUX_GROUP_DIR/merge_grants/pr-N`) — the grant dir is on disk under the agent's
  own uid, so its *existence* isn't cryptographically unforgeable; the "human-only" boundary is
  that no loomux surface (MCP) mints one, not that the filesystem forbids it. All the same class
  as absolute-path gh. Catching every shape needs sandboxing or network egress control, out of
  scope. **The gold-standard closing layer is a machine account**: give agents a GitHub token
  whose account has *no merge permission on the default branch and no release/tag-push rights*
  (branch protection + tag protection requiring a human/CODEOWNER). Then no client-side evasion
  matters — the server refuses. The shims are the cheap, local, always-on first layer; the
  machine account is the authoritative one, and the two compose.

## Human-input paste guard (#111)

The quiet backstop (#43, `wait_for_user_quiet`) only waits out *active* typing — it releases
the moment the human stops. It does **not** stop a paste landing on top of a line the human
typed and then **left sitting** in the box. When that happens the paste appends to their text
and the submit Enter fires the merged line: the live repro was a worker pane holding `/model`
(and later `dfgdsfg`) when a task delivery arrived, submitting `Unknown command: /modelRun …`
— the human's input consumed *and* the task destroyed. The stranded-flush guard (#81/#84) is
no help: it protects a *previous delivery's* text, not a *human's* fresh line, and explicitly
declines to flush once a human has typed.

So before the paste, delivery runs a second gate that distinguishes a sitting human line from
an empty box and holds/aborts rather than merge-submitting.

- **The signal — keystroke content, not output bytes.** Box occupancy is tracked from what the
  human *types*, which is the only thing that reliably tells a sitting line from a submitted one.
  Each human write (`write_pty`) is classified by the pure `classify_human_input`: printable text
  → `Content` (a line now sits in the box), an Enter / Ctrl-U / Ctrl-C → `Submit` (the box
  cleared), navigation/backspace/bare escape sequences → `Neutral` (occupancy unchanged). That
  updates a per-pane `input_pending` flag (`PtyManager::input_pending`). Delivery reads the flag;
  it does **not** look at output bytes.
  - **Why not an output-byte floor.** The first cut compared output growth since the last
    keystroke against a fixed 24-byte "burst" floor. It failed both ways: a single keystroke's
    input-line redraw in a full-repaint TUI — or the agent's own mid-turn streaming while a line
    sits — can clear the floor, so a still-sitting line reads as *submitted* and the paste
    merge-submits it (the exact #111 loss); and a *sub-floor* submit (empty Enter, short command)
    never clears the floor, so the box reads as dirty forever and every later delivery wedges in a
    60s hold. A keystroke's content has neither ambiguity: an Enter is positively a submit
    regardless of how few bytes it echoes, and ambient output never touches the flag.
- **The hold.** `hold_for_human_input` drives the pure `resolve_paste_gate(box_pending, held,
  max_hold)` each poll: `Paste` when the box is clear (or clears mid-hold, as the human submits),
  `Hold` while their line sits, `Abort` at the bounded cap (`HUMAN_INPUT_HOLD_MAX`, 60s). Same
  pure-gate-plus-testable-loop split as the quiet backstop (`should_hold_for_user` /
  `hold_until_quiet`), for the same #40 reason: exercise the loop, not just the decision.
- **The action.** On `Abort` the delivery pastes **nothing** and calls `notify_delivery_held`
  (gate `should_notify_paste_held`): one audited (`delivery-held-notice`) `[loomux]` notice
  (`paste_held_notice`) to the orchestrator — *"delivery to `<id>` held: pane has human input —
  re-send when clear."* Distinct from the unconfirmed notice: nothing landed, so the move is to
  wait for the box to clear and re-send, not to read back a stranded prompt. A cleared hold is
  audited (`delivery-held-for-input`) and proceeds normally.
- **No loops / paused.** Same discipline as #103: an orchestrator-target delivery never
  notifies (a notice to it is a delivery to it), and a **paused** group is skipped wholesale.
- **`last_user_input_ms` is stamped only on keystroke evidence (#496 PR-A).** Originally "every
  human write stamps it, unconditionally" — but `write_pty` runs on xterm's automatic replies to a
  program's terminal queries too (colour probes, focus reports, DA/CPR — #179's precedent), with no
  human present at all, and those replies classify `Neutral` with a zero `box_occupancy_delta`. A
  copilot pane that only ever emitted such replies could hold this clock at "now" forever with the
  human never touching the keyboard — which simultaneously deferred the autonomous idle tick's
  quiet window (#496), suppressed the stranded-flush guard below and the submit retries, and
  withheld Tier-1 confirmation, since all four read this same timestamp. `PtyManager::
  note_user_input` (the code `write_pty` now calls) gates the stamp on `classify_human_input`/
  `box_occupancy_delta` reading actual keystroke evidence (`Content`, `Submit`, or a nonzero delta —
  i.e. backspace/DEL) — reusing the #179 scanner rather than adding a second classifier. Tradeoff
  taken deliberately, and broader than "arrow keys, menu navigation" (#496 N2 review): Tab,
  Home/End/F-keys, Ctrl-A/E/W/K, and mouse-tracking/wheel CSI reports are all pure-`Neutral`
  zero-delta too, so a human wheel-scrolling a pane while merely *reading* output — not just
  navigating a menu — also stops refreshing the clock. Safe because the interactive-question
  guard's own release (below) is already STATE-based, not activity-based, for exactly this reason,
  and the idle tick is a notice, not an action. `input_pending` is a separate, additive flag written
  under the same `ptys` lock so the pair can't tear; it was already unaffected by this gate (its
  `Neutral` branch still applies `box_occupancy_delta` regardless of whether the timestamp stamps).
  **Per-write-completeness assumption (#496 N3 review):** the classifier is stateless per write, so
  a query reply fragmented across two `write_pty` calls would have its tail read in isolation and
  could stamp — unreached today (xterm synthesizes a reply as one `onData` event; the frontend's
  writer only splits above 16 KiB, far bigger than any auto-reply) and, even so, it fails toward the
  OLD behaviour for that one write, not toward #496's bug. Written down so a future CLI reply
  pattern that defeats it is findable, not rediscovered.
  **The `phantom-input-gated` breadcrumb is throttled per pane (#496 N1 review):** un-throttled, the
  same broadened `Neutral`-zero-delta class above hits this branch on every write while a human
  actively uses a pane — arrow-key/Tab spam or wheel-scroll bursts, each a separate file-open — and
  the flood would dilute (and could rotate out of the 2 MB `breadcrumbs.log`) the mid-session copilot
  signal the breadcrumb exists to capture (plan §7) before the one live validation run goes looking
  for it. `phantom_gate_tick` (pure; unit-tested with no filesystem involved, `pty.rs`) bounds
  emission to one line per pane per 5s interval, reporting how many gated writes coalesced since the
  last line so the rate stays visible. Restricting emission to panes with an orchestration identity
  was the preferred fix, but that would need this per-write hot path to reach into orchestration's
  agent registry from `pty.rs` — real new coupling, not just calling the already-shared classifier —
  so the throttle was chosen instead, staying inside `pty.rs` with no new command wiring.
- **Residual, and the #112 boundary.** Occupancy is inferred from keystrokes, not read from the
  box, so some cases still need true box-occupancy detection (issue #112). Splitting them by
  direction:
  - *False-negative (correctness — the dangerous direction), all fenced to #112:* an editor mode
    where Enter inserts a *soft* newline instead of submitting (a bare `\r` we'd read as a
    submit). Bracketed pastes are **not** in this set — a write carrying the `ESC[200~`/`ESC[201~`
    markers is classified `Content` regardless of any interior/trailing newline, so a pasted line
    ending in `\n` is not misread as submitted.
  - *False-positive (availability only — a stuck `input_pending`), each bounded by the 60s
    hold → abort → one held-notice → orchestrator re-send, and cleared by the human's next
    Enter/Ctrl-U/Ctrl-C:* **any** box-clear that isn't a trailing newline / Ctrl-U / Ctrl-C —
    Esc-to-clear (common in Claude Code), Ctrl-W (delete word), Ctrl-K (kill to end), and
    backspace-to-empty. These resolve to `Neutral` (they add no visible text), so a box the human
    emptied that way still reads as pending until the bounded abort.

  The guard errs toward the safe hold in the common case; this is the paste-path guard only — the
  confirm-window semantics (`submit_confirmed` and false-confirm handling) are #112, deliberately
  untouched here.

## Attention routing (#6) & interactive-question detection (#40)

The human is the scheduler's bottleneck; attention routing surfaces *which* pane needs
them so they don't scan panes. A background loop (`start_attention`, 3s tick) reads a pty
snapshot and hands it to the pure `attention_tick`, which emits an `AttentionItem` per pane
that needs the human, with a reason in priority order: `blocked` (reported) > `waiting`
(parked on a prompt) > `report` (reported done) > `gate` (the pane's board task sits at a
merge gate). Keeping the policy pure w.r.t. the pty (the pty reads live in
`attention_inputs`) makes the whole thing fixture-testable with synthetic maps — no real
CLI. The frontend routes each item by `pty_id` to `Pane.setAttention`, which paints the
header chip and, via a listener, mirrors the state onto a minimized pane's **dock chip**
(`Grid.renderDock` → `dockChipAttention`) so docking never hides an ask.

- **Scope: every pane, not just agents (#40).** The `waiting` reason applies to *any* live
  pane, including a plain shell the human opened by hand to run a CLI — those have no
  orchestration group/roster identity, so the original agent-only scan never saw them (the
  human's repro: two hand-opened panes running Claude Code / Copilot, both parked on a
  question, no indicator anywhere). `run_attention` now makes two passes: `attention_tick`
  over the roster (all four reasons), then `plain_pane_attention` over every *non-agent* live
  pty (`PtyManager::live_ids`), which raises only `waiting`. Plain-pane items carry just
  `pty_id` (empty `agent_id`/`group`, `role: None`) and are keyed in the shared
  `attn_quiet`/`attn_waiting_ack` maps by a synthetic `pty:<id>` id. The frontend badges **any**
  pane by `pty_id` (the old `orchGroupId` gate is gone); a plain pane acks by pty id
  (`orch_ack_attention_pty`) since it has no agent id. Agent-only surfaces — board-row
  highlight, desktop toasts — stay group-scoped by construction (a plain pane's empty group
  is in no opted-in set), which is the intended split: any blocked CLI lights the pane chip
  and dock dot, while the richer group features remain orchestration-only.

- **The `waiting` heuristic.** A pane is `waiting` when its output has been quiet past
  `ATTENTION_QUIET_MS` (4s), there's been no recent human keystroke, *and* its ANSI-stripped
  tail looks like a live interactive prompt (`prompt_wait_detected`). The quiet + no-keystroke
  gate is what separates a *live* prompt the human must answer from the same words scrolled
  past or a prompt the human is already typing into.
- **The tail read is BOUNDED, because it happens under the keystroke path's lock (#717).**
  Every pane's tail is read through `attention_tail`, which asks
  `PtyManager::output_tail_bounded` for the trailing `ATTENTION_SCAN_BYTES` (4096) and strips
  ANSI *after* the read has returned, so nothing is scanned under a lock at all. It used to
  clone the whole (up to 256 KB) ring and slice the same 4 KB off the result — a 64x copy on a
  saturated ring, taken while holding the global `ptys` mutex that `write_pty` /
  `note_user_input` take on every keystroke and behind which the pane's own reader thread
  queues to append its next chunk. What the detectors see is byte-identical either way
  (`strip_ansi(&ring[len-N..])` *is* `strip_ansi(&last_N)`), so the heuristic is untouched;
  only the hold changed. That identity is also why no assertion on the scan's *output* can
  police this, and why `attention_tail` and `pane_attention_inputs_from` take the raw reader
  as a closure rather than a pre-read `Vec<u8>`: the size of the request is the only thing a
  test can reach — `Tier1Scan::read`'s precedent, for the same reason.
- **#40 — questions weren't detected.** `prompt_wait_detected` originally only fired on a
  selection glyph that *starts* an option line (`starts_with('❯')`), a `1. yes` numbered menu,
  explicit `y/n` tokens, or a fixed list of permission phrasings. Two real interactive-question
  styles slipped through, so the pane chip **and** the dock dot both stayed dark:
  - **Claude Code `AskUserQuestion`** highlights the active option with *reverse-video* (an
    ANSI attribute stripped before detection sees it), leaving numbered options with arbitrary
    labels and no glyph — nothing in the old list matched. Fix: recognize the interactive
    selection-menu **footer** (`enter to select`, `enter to confirm`, `use arrow keys`,
    `↑↓`/`↑/↓`), which survives stripping.
  - **Copilot CLI** draws its `❯` pointer indented inside a bordered box (`│ ❯ Yes`), so the
    option line never *starts* with the pointer after trimming. Fix: strip a line's leading box
    frame / bullet before checking that a `❯`/`›`/`→` pointer *leads* it.
- **Two signal tiers, to avoid a false-positive storm.** The tricky part (#40 review): the two
  new signals are *prose-like* — agents routinely write about keyboard UIs ("use arrow keys…"),
  paste shell prompts (`demo ❯ npm run dev`), and echo `a › b` breadcrumbs, and a *finished*
  agent stays output-quiet with that text in its tail indefinitely, so the quiet gate alone
  does not save them. So the signals are split by how prose-safe each is:
  - *Structured* signals (numbered `y/n` menu, `y/n` tokens, stock permission phrasings) don't
    occur in ordinary prose → honored across the last ~12 lines.
  - *Prose-like* signals — the selection pointer and the plain-English footer — are both read
    **only from the last ~3 non-empty lines** ("the last thing painted"), and the pointer must
    additionally *lead* a de-framed line. A live menu paints its pointer/footer last; a finished
    turn is followed by the CLI's redrawn idle input box, which pushes any pointer/phrase earlier
    in the tail out of range. This is what rules out both a *mid*-line glyph (`demo ❯ npm run
    dev`, a `Home › Prefs` breadcrumb) **and** a *leading* one in finished prose (a `❯ npm run
    dev` repro line, a fenced `❯` command block) above the idle box. The Copilot positive still
    passes on its footer (its boxed pointer sits above the last-3 window); the Claude positive on
    its footer; and a bare inquirer `❯` prompt passes on the pointer when it *is* the last line.
  - Covered by fixtures under `src-tauri/tests/fixtures/attention/`: three positive question
    styles (Claude footer, Copilot footer, bare-pointer-last-line) and **seven** negatives — a
    numbered summary stream, an idle input box, and the five finished-turn-prose repros from the
    review (keyboard-nav prose, mid-line `❯` shell prompt, `›` breadcrumb, leading-`❯` repro
    steps, fenced-`❯` block) — all run through the real `strip_ansi` → `prompt_wait_detected` →
    `attention_tick` path.
- **`waiting` ack is sticky (`attn_waiting_ack`).** `blocked`/`report` latch until acked;
  `waiting` is recomputed live each scan, so without care, focusing a pane whose menu is still
  on screen would clear the chip only to have the next 3s scan re-light it. So acking a pane
  (`ack_attention`, fired when the human turns to it) records it in `attn_waiting_ack`, which
  suppresses `waiting` for that pane **until its output next changes** — i.e. the menu was
  answered or the CLI repainted, at which point it re-arms and a genuinely new prompt flags
  again. This makes "turn to a pane → it stops nagging" hold for `waiting` the same way ack
  clears `blocked`/`report`, while still catching a fresh question later.
- **Known limits.** The footer match is per-line, so a footer wrapped across rows in a very
  narrow pane, or a **localized / reworded** footer, won't match — acceptable for now (the
  pointer and structured signals still cover most such cases). The quiet gate is load-bearing:
  a menu that keeps emitting bytes (blinking cursor, live countdown) never goes quiet and so
  never flags; today's targets (static AskUserQuestion / Copilot menus) do go quiet. Anchoring
  the pointer to the last 3 non-empty lines also means a **footer-less** menu whose ❯ sits at
  the top with 3+ options below it is missed until the user arrows down (the pointer re-enters
  the window); real menus ship footers, so this is a safe-direction miss we accept.

## Interactive-question paste guard (#420)

**Problem.** Copilot (and Claude Code) surface interactive questions — a numbered/radio-select
menu, a y/n permission prompt, `AskUserQuestion` — as a TUI dialog, not as text sitting in the
input box, so neither existing guard sees it: the quiet backstop (#43) only tracks keystroke
recency, and the box-occupied guard (#111) only tracks keystroke *content*, and a live menu isn't
either. When a programmatic delivery (a worker report, a kickoff, a notify-tick notice, a compact
nudge — any `deliver_prompt` call) lands while such a dialog is up, the bracketed paste is
harmless (menus generally ignore stray text), but the submit **Enter is not** — it doesn't merge
text like the box-occupied case, it **selects** whichever option is currently highlighted (usually
the first/default). That's worse than losing the delivery: it silently answers a question with an
answer nobody chose and steers the agent's turn in an unintended direction.

**Detection — reuse, don't reinvent.** `prompt_wait_detected` already exists for exactly this
question-is-on-screen problem, built for attention routing (#6/#40, see above): it keys on
*stable structural markers* — a numbered `1. yes` / `❯ 1.` menu, `y/n` tokens, stock permission
phrasings, or (the harder cases it was built for) a selection pointer that *leads* a de-framed
line, or a plain-English menu footer (`enter to select`, `use arrow keys`, `↑↓`) read from only
the last few painted lines — rather than trying to parse option *text*, which is exactly the
"prefer stable markers over parsing options" shape this guard needs too. It already covers
Copilot's boxed/indented pointer and Claude's reverse-video `AskUserQuestion` (whose highlighted
option carries no surviving glyph, only the footer). `copilot-multichoice-question.txt` covers a
genuine *multi-choice* (non-yes/no) Copilot question whose pointer has scrolled out of the
last-3-lines window (a trailing status line follows it) — built to exercise `has_numbered_menu`'s
whole-window substring check specifically, distinct from the pointer/footer signals the other
three positive fixtures already pin (rev-15 N3: an earlier version of this fixture matched via the
SAME two signals `copilot-question.txt` already covered and so proved nothing new).

**Fixture provenance — real where reality is reachable (rev-19 N11).** Every Copilot-flavored
fixture in this set is a plausible, hand-built approximation of Copilot's TUI style, not a verified
live capture: an exhaustive search of this repo's history and every loomux group's audit-log archive
on the machine this fix was built on turned up no real captured Copilot multi-choice menu anywhere,
and CLAUDE.md constraint 3 forbids spawning a real Copilot CLI to obtain one in-session. Rather than
leave `has_numbered_menu` proven only against an imagined paint, `claude-mcp-approval.txt` grounds it
against a REAL one: a verbatim capture of Claude Code's MCP-server-approval dialog from a live
session's own audit log (respaced only — the `get_output` MCP tool's logged `text` field collapses
whitespace runs, an artifact of that logging path, not of the terminal itself; wording, numbering,
and footer are exactly as captured). It's Claude Code's, not Copilot's — but `prompt_wait_detected`
and `has_numbered_menu` are CLI-agnostic by construction (the whole guard is, per N5), so a
verified-real multi-choice menu grounds the signal against reality regardless of which CLI painted
it. **Correction (rev-19 n2):** this capture's footer survived intact too, so it witnesses `has_
numbered_menu` combined WITH `has_menu_footer`, not `has_numbered_menu` alone — it is not a
sole-signal witness, and the fixture/test comments were fixed to say so plainly rather than imply
otherwise. Sole-signal isolation for `has_numbered_menu` specifically is still what
`copilot-multichoice-question.txt` is for (synthetic, footer/pointer deliberately absent, scrolled
out of the prose-guard's last-3-line window) — the two fixtures are complementary, not redundant:
one proves the combined real-world shape, the other proves the one signal that shape's footer would
otherwise mask ever being exercised alone.

**What this detector does NOT catch (rev-15 N3).** `prompt_wait_detected` is structural-marker-only
by design (see above) — a **free-text ask** ("What should I name the new module?") carries none of
those markers, so a delivery lands on top of it and the pasted text is submitted *as the answer to
the free-text question*, not held. A footer wrapped across rows in a very narrow pane doesn't match
(noted at the detector's own doc comment); a Copilot release that rewords its footer/phrasing
silently drops that signal until the fixtures and detector are updated to match. This guard reduces
the "silently pick the highlighted option" hazard for the *menu/permission* shape it can see
structurally; it is not a general "loomux will never type over Copilot" guarantee.

**Hold — a single injectable predicate, checked at every submit-equivalent write (rev-15 B1–B4).**
The first version of this guard added two checkpoints (pre-paste, pre-Enter) but left the two OTHER
places `deliver_prompt` presses Enter — the stranded-text flush (#81/#84) and the spaced submit
retries (#98) — completely unguarded, on the review's own account: *"`deliver_prompt` presses Enter
into the pane in three places, and this PR guards one of them. The other two are the ones that fire
while an agent is mid-turn — i.e. exactly when Copilot paints a question."* Concretely: the flush's
Enter is the FIRST write a delivery makes, before either checkpoint runs, so a question already on
screen from before the delivery started would eat it (B1); the two spaced retries (`SUBMIT_RETRY_
DELAYS`, +2.5s/+7s after the first Enter) fired unconditionally, in exactly the window Copilot most
often paints its permission dialog after processing a submitted prompt (B2). Both are now gated
through the SAME mechanism as pre-paste/pre-Enter, not a bespoke check each:

- `question_hold_predicate(tail, pasted_text) -> impl Fn() -> bool` is the core decision, generic
  over the tail read (no `PtyManager` bound into it) — the fix for B4's finding that the OLD
  `wait_for_question_clear` bound a concrete `&PtyManager`, so a test that fully disabled the
  guard's predicate still passed the entire suite (495/495) unchanged: nothing exercised the
  *wiring*, only presentation/notice text. This generic form is driven directly by tests with
  scripted closures — no PTY, no app handle — closing that gap; disabling either remaining signal
  (the content mask, the detector) now fails a dedicated test, confirmed by neutralizing each
  locally and re-running (rev-19's own mutation method, turned on the fix itself).
  `pasted_text: Option<String>` replaced an earlier `paste_baseline_total: Option<u64>` byte-count
  parameter — see B-A below for why the growth-delta approach was replaced with content-masking,
  not just re-timed.
- `wait_for_question_clear(ptys, pty_id, pasted_text, emit_held, emit_held_cleared)` binds it to a
  live pane and reuses `hold_for_human_input`'s generic block-until-clear-or-capped loop verbatim
  (same cap `QUESTION_HOLD_MAX` = 120s, longer than the box guard's 60s: reading and deciding a
  substantive question takes more of a human's attention than submitting an already-typed line) —
  called at the pre-paste checkpoint, the pre-Enter checkpoint, AND now each spaced retry (via
  `retry_gate`, below, and gated on `!confirmed` — see N8), so all four Enter-adjacent sites share
  one hold implementation instead of four independently-reasoned checks.
- `question_active_now(ptys, pty_id, pasted_text)` is the one-shot (non-holding) form used by the
  stranded-text flush: a plain snapshot, not a wait, because holding there would be redundant — if
  a question is active, the flush is skipped and the pre-paste checkpoint immediately following it
  is the one that actually holds/aborts/notifies.
- `flush_stranded_text(ptys, pty_id, prev_confirmed, human_typed_since, submit) -> bool` (rev-19
  R3) is the flush's check-AND-write as one function — `deliver_prompt` calls it directly, so an
  integration test driving THIS function (not a reimplementation of its logic) against a real
  `PtyManager` proves the wiring, not just the decision. `record_aborted_preenter_outcome`/
  `recorded_confirmed` are the analogous extraction for the pre-Enter abort's outcome-record (B3):
  the former is what `deliver_prompt`'s abort branch now calls instead of inlining the
  `last_delivery.insert`, the latter mirrors the exact read `deliver_prompt`'s flush step performs
  (`prev.as_ref().map(|o| o.confirmed)`). Both are exercised against
  `PtyManager::register_fake_for_test` (pty.rs) — a REAL ConPTY pair + a trivial spawned child (so
  `master`/`killer` are genuine) whose output ring is manually seeded and whose writes are captured
  into a plain buffer, letting an integration test drive the exact `PtyManager` methods
  `deliver_prompt` calls without a real Tauri `AppHandle` (unavailable headless — `tauri::test`'s
  `MockRuntime` isn't the concrete `Wry` runtime production `spawn_pty` requires) or a real agent
  CLI (CLAUDE.md constraint 3 — the fake's child is a bare `cmd`/`sh` no-op, never an agent).
  **Residual, stated plainly:** these tests prove `flush_stranded_text`/`record_aborted_preenter_
  outcome` behave correctly, and (for the flush) that `deliver_prompt`'s call site can't be
  half-bypassed internally — but nothing in this codebase's test infrastructure can prove
  `deliver_prompt` itself calls `record_aborted_preenter_outcome` at its one call site (deleting
  that single line reds no test, confirmed by trying it and reverting) — that specific link is
  code-review evidence only, the same boundary every other piece of `deliver_prompt` already
  accepts (see "Tests" below and the design note this section opens with).
- `should_flush_before_paste_now(prev_confirmed, human_typed_since, question_active)` (B1) and
  `retry_gate(question_decision) -> RetryGate` (B2, rev-19 N9: no longer takes a human-typing flag
  — see below) are named pure decisions — not inline `&&`s at the call sites — so the
  flush-suppression and the retry outcome are independently testable and can't silently drift apart
  from each other on a future edit.

**Release is STATE-based, not ACTIVITY-based (rev-19 R1, replacing the original N2 fix).** The
first cut released the hold on a human keystroke (submitted, not left sitting) landing after the
hold began. Round 3's review found this wrong on both ends: not sufficient — an arrow key
*navigating the still-open menu* (`classify_human_input` reads it as `Neutral`, no text left
sitting, since it doesn't add or submit anything) satisfied the old condition and let the freed
Enter fire straight into the still-open dialog — and not necessary, since xterm's own automatic
terminal-query replies (#179's own precedent: `ESC]11;rgb:...`/DCS `XTVERSION` replies) can stamp a
pane's keystroke-recency clock with no human present at all. **Human activity is dropped from the
decision entirely.** `question_hold_predicate` now takes no input-related parameters whatsoever —
the only thing that can say the question is gone is the SAME detector that said it was there:
`prompt_wait_detected` reading false. A single false read isn't trusted alone (a redraw mid-flicker
could transiently miss the menu) — release requires it read false on `QUESTION_RELEASE_CONSECUTIVE_
CLEAR_POLLS` (2) consecutive polls. This only engages once a hold has genuinely observed a question
at least once (`ever_shown`, internal `Cell` state) — a checkpoint that starts already-clear
releases on its very first check, no artificial delay (rev-19 N10; pinned by
`question_hold_predicate_wired_into_the_generic_hold_loop_releases_immediately_when_already_clear`,
which asserts `held_ms == 0` through the REAL `hold_for_human_input` loop, not just the predicate in
isolation). This is deliberately a lighter mechanism than attention routing's `waiting_ack`/
quiet-gate machinery (built to debounce a *frontend badge* against redraw flicker across a much
longer observation window) — this guard only needs "is the menu still painted right now", which the
detector itself already answers without any human-input signal at all.

**#496 PR-A tightened the clock this section distrusts, without changing this decision.**
`PtyManager::note_user_input` now gates `last_user_input_ms` on `classify_human_input` itself, so
an xterm auto-reply no longer stamps it (see the box-occupancy section above). That does not make
release activity-based again: the "not sufficient" half of the finding — an arrow key navigating a
still-open menu reads `Neutral` too, and #496 doesn't stamp pure-`Neutral` input either — is
untouched by tightening what the clock tracks. `question_hold_predicate` stays exactly as-is.

**Self-echo is excluded by CONTENT, not by byte-count delta (rev-15 N1, rewritten rev-19 B-A).** The
pre-Enter and retry checkpoints necessarily read the tail AFTER loomux's own bracketed paste — so a
delivered prompt that happens to contain phrasing `prompt_wait_detected` matches (`(y/n)`, "do you
want to run", "1. yes" — ordinary agent-to-agent traffic: *"copilot asked `Do you want to run npm
test?` and I can't answer"*) would otherwise hold the delivery on **itself**, deterministically and
unrecoverably, since re-sending the identical text reproduces the identical false match.

Two earlier rounds tried to solve this with a byte-count GROWTH baseline (rev-15: snapshot the
output total right after the paste, hold only if it grew since; rev-19 round 3/R2: move the snapshot
to after the pane genuinely settles, extending the wait if growth hadn't reached the pasted byte
count). **Both were wrong in the same way, proven by a scratch test against a real `PtyManager`:** a
byte-count baseline is ALWAYS one number marking ONE point in time as "before" — it cannot represent
"this delta is our own paste, that delta is their dialog" when both arrive in the same window. The
canonical #420 timeline is exactly that: loomux pastes, Copilot processes it and paints a dialog
*while the paste is still settling* — whichever moment the checkpoint snapshots, the dialog either
gets baked into the baseline (round 3, `grew == false`, invisible forever) or the growth check
passes for the wrong reason. Moving WHEN the snapshot is taken cannot fix a comparison that is
structurally the wrong axis.

The actual fix (rev-19 B-A): `deliver_prompt` knows the EXACT text it pasted (`text: &str`, captured
into the delivery thread as `pasted_text`). `mask_own_paste(tail, pasted_text)` removes every line of
the tail that exactly matches one of `pasted_text`'s own lines (trim + lowercase compared, matching
`prompt_wait_detected`'s own line normalization) BEFORE the detector ever runs. `question_hold_
predicate` takes `pasted_text: Option<String>` instead of a growth baseline — `None` for the
pre-paste checkpoint (nothing of ours is on screen yet, so nothing to mask); `Some(the pasted text)`
for pre-Enter and every retry. A paste whose own prose happens to contain "(y/n)" masks itself away
to nothing (every line of the tail IS a pasted line); a dialog painted alongside — or during, or
after — that same paste survives masking untouched, because its lines are NOT among the ones we
pasted, regardless of the exact moment it rendered. This removes the timing dependency entirely: no
snapshot, no settle-wait, no window to get baked into. The now-unused settle-baseline machinery
(`paste_write_before`, `PASTE_SETTLE_EXTRA_WAIT`) was deleted along with it — the underlying "wait
for the pane to go quiet before Enter" loop stays, since `submit_confirmed`'s `reached_quiet` still
needs it for an unrelated reason (rev-32).

Content-masking has its own honest limit: it depends on the tail rendering our OWN lines closely
enough to match (trim + lowercase). A CLI that reformats a long pasted line differently than we sent
it (aggressive re-wrapping, whitespace normalization beyond trim) could leave a residual, unmasked
fragment of our own text — a narrower, DIFFERENT failure mode than delta-based baselines had, not
eliminated in principle, but no longer structurally guaranteed to miss the canonical #420 timeline
the way a byte-count comparison was. Pinned by `question_hold_predicate_on_a_real_ptymanager_holds_
for_a_dialog_seeded_alongside_a_large_paste_but_not_for_the_pastes_own_matching_text` — rev-19's own
scratch-test shape, run against a real `PtyManager` (not scripted closures), reproducing exactly the
regression the review found: a seeded dialog alongside a large paste holds; the same large paste
alone, its own text matching the detector, does not.

**Retries skip entirely once the submit is confirmed (rev-19 N8).** A CONFIRMED delivery is done —
its Enter landed, its turn started, there is nothing left to retry. Running the retry loop (and its
question-hold machinery) regardless used to be harmless under the ORIGINAL "Enter on an empty box is
a no-op" premise (`SUBMIT_RETRY_DELAYS`'s own comment) — the premise this PR's entire existence
disproves. Concretely: a successful, CONFIRMED delivery to a Copilot pane that then asks an
unrelated question (nothing to do with this delivery) would show a false "held: question pending"
badge and hold the pane's per-pty delivery mutex for up to `QUESTION_HOLD_MAX`, blocking every OTHER
delivery queued behind it — for a delivery that already landed. `deliver_prompt`'s retry loop is now
wrapped in `if !confirmed { ... }`: this guard protects deliveries in flight, it is not a general
question-watcher for a pane that's already done receiving this one.

**No dead `unreachable!()` in thread code (rev-19 N9).** `RetryGate` used to carry a
`SkipHumanTyping` variant the caller could never actually reach (the retry loop already `break`s on
human-typing BEFORE calling `retry_gate` at all), matched with an `unreachable!()` — a latent panic
sitting in a detached thread, waiting for some future reordering of the caller to make it reachable
for real. `retry_gate` now takes only the question decision and has exactly two outcomes
(`Write { held_ms }` / `SkipQuestionPending { held_ms }`, carrying the duration so the caller never
needs to re-destructure the original `PasteDecision` either) — human-typing precedence is enforced
entirely by the caller's own early `break`, which is structurally guaranteed correct (not "checked
above" trusted by comment) rather than something a match arm could get wrong.

**The test harness must not be able to leak a real process (rev-19 B-B).** `PtyManager::register_
fake_for_test` (pty.rs, backing the R3 real-PTY tests above) opens a REAL ConPTY pair + spawns a
trivial child so `master`/`killer` are genuine — the first cut spawned `cmd.exe /c pause>nul`
(Windows) with `_job: None`, i.e. a command that waits for input FOREVER, enrolled in NEITHER the
kill-on-close Job Object (#78) NOR anything else that would tear it down if a test panicked or the
process was itself killed mid-run. rev-19 found 13 orphaned `cmd.exe` + 13 `OpenConsole.exe` on the
machine this was built on (11 traced to this worktree), holding `target/debug` open and failing the
next `cargo build` with "used by another process" — exactly the local-machine damage class the
resource discipline exists to prevent; CI never sees it (fresh runners each time), which is why it
went unnoticed there. Fixed with two independent layers, matching production `spawn_pty` rather than
inventing a test-only shortcut: (1) the spawned command now exits immediately on its own (`cmd /c
exit 0` / `sh -c true`) — nothing in this harness needs the child to stay alive, since `write_bytes`/
`output_tail`/`output_total` never touch it, only `master`/`killer`, which just need to be real
objects to satisfy `PtyHandle`'s fields; (2) on Windows, the SAME kill-on-close Job Object production
spawns use (`assign_kill_on_close_job`) is assigned here too, so even if a test panics mid-run,
dropping the `PtyHandle` (however that happens) closes the job's last handle and the kernel reaps the
subtree — structural, not dependent on any test's own cleanup code running. Verified: a full
`cargo test --locked -j 4` run (all binaries) shows zero `cmd.exe` and zero non-application
`OpenConsole.exe` processes before and after.

**Cost (rev-15 N4).** Each checkpoint used to clone the FULL (up to 256 KB) output ring and
`strip_ansi` all of it every 250ms poll, for up to 120s, across up to four sites per delivery.
`PtyManager::output_tail_bounded` reads only the trailing `QUESTION_SCAN_TAIL_BYTES` (4096, matching
the attention path's own trailing window, `ATTENTION_SCAN_BYTES` — which since #717 is the size of
that path's *read*, not just of the slice it took afterwards) directly off the
back of the ring's `VecDeque` — `O(bounded bytes)`, not `O(ring length)` — cutting both the clone and
the `strip_ansi` pass to a fixed, small size regardless of how much the pane has ever printed.

**Latency and ordering (rev-15 N7, documented not re-engineered).** The worst-case hold chain for a
single delivery now stacks the human-quiet backstop (up to `USER_QUIET_MAX_HOLD` = 90s), the
box-occupied guard (`HUMAN_INPUT_HOLD_MAX` = 60s), and the interactive-question guard at the
pre-paste and pre-Enter checkpoints (`QUESTION_HOLD_MAX` = 120s each) — several minutes in the
pathological case where each condition is independently true and re-triggers at the next
checkpoint. The retries add a further, BOUNDED exposure only while `!confirmed` (rev-19 N8 caps
this to the unconfirmed case, not every delivery): each spaced retry can also hold up to
`QUESTION_HOLD_MAX` before giving up, though in practice a still-unconfirmed delivery reaching the
retries has already survived the pre-Enter checkpoint, so this rarely compounds rather than
replaces that hold. Deliveries are serialized
per pane by the existing `delivery` mutex (`std::sync::Mutex`, not FIFO on Windows — see the #43
section's own "Ordering is best-effort" note, which this compounds rather than introduces), so N
queued deliveries to the same pane can each stall for up to this chain. **Superseded by #445/#470:**
the "release out of send order" consequence this paragraph originally accepted no longer holds — the
delivery QUEUE (not this mutex) is what now owns arrival order, structurally, regardless of how long
any individual hold in the chain above runs; see "Delivery queue (#445)"'s "Ordering" subsection.
This mutex remains exactly what it always was for the *latency* concern this paragraph is about
(each delivery can still individually stall for the full chain) — only the ordering consequence is
superseded.

**Orchestrator-target hold is silently dropped (rev-15 N6, pre-existing, more reachable now).**
`should_notify_paste_held(true)` suppresses the held-delivery notice when the target IS the
orchestrator (a notice to it would itself be a delivery to it — an endless loop, same discipline as
#103's unconfirmed notice). This predates #420 (#246/#111), but the orchestrator pane is
disproportionately likely to be the one running an interactive CLI a human watches directly (and,
per the detector's own coverage, likely running Claude Code's `AskUserQuestion`) — so a
question-held delivery TO the orchestrator now has no surfacing mechanism at all beyond the pane's
own badge. Fixing this needs its own design (some UI-only "held, no notice" surfacing distinct from
the notify-the-orchestrator path, since notifying the orchestrator IS the thing that can't happen
here) and is out of scope for this PR; noted so it isn't mistaken for solved.

**The autopilot-consent dialog is deliberately exempt — by construction, not a special case.**
The `confirm_copilot_autopilot_dialog` watcher (#101/#179/#364, above) *answers* a specific dialog
on purpose — that is its entire job, and it must keep doing it even though it looks, from the
outside, exactly like the hazard this guard exists to prevent. Three of the four submit-equivalent
sites (flush, pre-paste, pre-Enter) are exempt without any explicit skip because of *when* they
run: strictly *before* `deliver_prompt` sends the kickoff Enter that Copilot's autopilot dialog only
appears in response to (verified live: a fresh `--autopilot` pane paints a normal input box, not the
dialog, until that first submit). The autopilot confirm itself runs *after* that Enter, as a
separate, targeted watch-and-answer step scoped to `copilot_autopilot_prompt_detected` — a narrower,
differently-anchored detector than `prompt_wait_detected` — so even if those checkpoints ran later
than they do, they key off different text and would not fight over the same dialog. Both facts are
recorded as a comment on `HeldReason::InteractiveQuestion` and at the confirm call site so a future
change to either ordering doesn't silently reintroduce a collision between "hold for the human" and
"answer this one dialog on the human's behalf".

The FOURTH site — the spaced retries — runs after the autopilot confirm's own watch window
(`AUTOPILOT_DIALOG_WAIT`, 12s) has already elapsed, so no live collision: the confirm has either
already answered the dialog or already given up by the time a retry could see it. **Residual worth
documenting (rev-15, "verified sound" but flagged):** if the dialog appears late enough that the
confirm's fail-soft window misses it, retries now HOLD on it (via the same general guard) instead
of pressing an Enter through it the way they used to — an unattended `--autopilot` pane can now
stall at the dialog for up to `QUESTION_HOLD_MAX` and abort, where before this PR a lucky blind
retry might have self-consented past it. This trades a rare stall for never risking a retry landing
on the WRONG option of some OTHER dialog that happens to be up at the same moment — judged the
correct direction for the same reason the rest of this guard exists.

**Tests.** Pure-decision coverage: `prompt_wait_detected_fires_on_interactive_question_fixtures`
(detector, including the rev-15 N3 and rev-19 N11 fixtures); `question_hold_predicate_*` —
self-echo masking, pre-paste's no-mask case, closed-pty/no-match cases, activity-can-never-release
(rev-19 R1 — `question_hold_predicate_ignores_activity_the_menu_is_still_open` polls a
still-matching tail five times with no way to feed it an input signal, since the parameter no
longer exists), two-consecutive-clear-reads-to-release, and immediate release when never shown a
question (rev-19 N10) — each reds if its corresponding signal is removed from the implementation,
verified by neutralizing it locally and re-running (rev-19's own mutation method, run against the
fix itself before every push). `mask_own_paste_removes_exactly_our_own_lines`/`..._leaves_nothing_
when_the_tail_is_only_our_own_paste` cover the masking function directly; `question_hold_predicate_
on_a_real_ptymanager_holds_for_a_dialog_seeded_alongside_a_large_paste_but_not_for_the_pastes_own_
matching_text` (rev-19 B-A) reproduces rev-19's own scratch-test shape against a REAL `PtyManager` —
the exact regression the review found (a dialog seeded alongside a large paste holds; the same large
paste alone, its own text matching the detector, does not) — reverting the masking to a bypass reds
both this test and the self-echo test above, confirmed locally. `should_flush_before_paste_now_is_
suppressed_by_a_live_question` (B1); `retry_gate_holds_for_a_question_or_writes_once_clear` (B2/N9);
`a_recorded_unconfirmed_outcome_makes_the_next_deliverys_flush_fire` (B3's downstream consequence).
Wiring coverage: `question_hold_predicate_wired_into_the_generic_hold_loop_releases_once_the_menu_
clears_twice` and `..._releases_immediately_when_already_clear` drive the REAL `hold_for_human_input`
loop with the real predicate (#40's own lesson: exercise the loop, not just the pure decision) —
matching the box-occupied guard's own `output_growth_never_flips_input_pending`-style coverage of
the identical generic loop. `flush_stranded_text_does_not_enter_when_a_question_is_showing`/`..._
enters_when_no_question_is_showing` and `record_aborted_preenter_outcome_makes_the_next_deliverys_
flush_actually_fire` (rev-19 R3) drive the ACTUAL functions `deliver_prompt` calls against a real
`PtyManager` (`register_fake_for_test`), proving the flush's wiring end to end and the abort-record's
downstream consequence through the real recorder/reader pair, not a fabricated literal (rev-19 n1:
this pins the recorder's behavior via the real extraction, NOT that `deliver_prompt` calls it — see
below for that honest gap).
`delivery_held_event_names_the_pane_and_the_reason` covers the `HeldReason` variant end to end
through the audit/badge payloads (the LIVE "still holding" badge, unaffected by #445 below).
**#445 update:** the notice fired once a hold's cap actually expires no longer says "held ... —
re-send when clear" and is no longer named `held_delivery_notice`/`paste_held_notice`/
`question_held_notice` — those were deleted along with the destroy-on-abort behavior they
described. `notify_queue_fires_for_a_worker_but_suppresses_and_audits_for_the_orchestrator_and_
while_paused` and `queued_notice_replaces_the_deleted_re_send_wording` cover the replacement (the
"Delivery queue (#445)" section, below, is the full account). What
remains untested, and stated plainly rather than silently accepted (per CLAUDE.md constraint 3 and
the "no real PTY in test mode" convention the rest of `deliver_prompt` already lives with): that
`deliver_prompt`'s call site for `record_aborted_preenter_outcome` (a single line) is actually
present — deleting it reds no test, confirmed by trying it and reverting — and that the retry loop's
`if !confirmed { ... }` wrap (N8) is correctly placed. Both are code-review evidence, the same
boundary every other guard in this function already accepts; the flush's equivalent wiring gap is
closed (see `flush_stranded_text` above) because its check-and-write collapse into ONE function with
nothing left for `deliver_prompt` to get wrong beyond calling it.

### #532: both safety signals fired backwards, and why that was one bug

**The report.** A pane showed **held: question pending** on a **totally empty** prompt box. Then,
as the human began typing their next message, the queue dequeued the earlier held prompt and
**submitted it mid-typing, over their input**. A false positive on one guard, then a missed true
positive on the other, in sequence.

**They are the same event.** `prompt_wait_detected` reads `PtyManager::output_tail_bounded`, which
is an **append-only byte ring, not a screen** — the last `QUESTION_SCAN_TAIL_BYTES` (4 KB) of
everything the pty has ever emitted. On a pane that has gone quiet, an *answered* question stays
inside that window indefinitely, and the structured signals the detector honours across its
12-line window (`(y/n)`, `do you want to proceed`, `1. yes`) keep matching. The detector's own doc
says as much — *"this alone can't tell a live prompt from the same words scrolled past"* — and
directs the caller to pair it with an output-quiet check. The attention scan (#6/#40) pairs it.
The late monitor pairs it (`quiet_long_enough &&`). **The #420 delivery hold — the one that blocks
writes — pairs it with nothing.** That is the false positive.

What clears such a stale reading is *fresh bytes*. When the human starts typing, their echo shifts
the ring and pushes the question out of the detector's window. So **the keystroke that releases
the question gate is the same keystroke that occupies the box.** `deliver_now` checked box
occupancy (#111) *first* and the question (#420) *second*, and the second one blocks for up to
`QUESTION_HOLD_MAX`. Nothing re-read occupancy afterwards, so the delivery pasted on a green light
earned two minutes earlier against a box that was empty *then*. The pre-Enter quiet wait then
submitted the merged line the moment the human paused. This is the ordinary interleaving, not a
rare race.

**Fix 1 — the gates are re-verified together, at the write.** `write_admission(box_pending,
question_active)` is the one derivation of "may this delivery write right now", replacing three
inline spellings (`deliver_now`'s pre-paste pair, and `run_queue_drainer`'s deliverability
pre-check). `box_pending` is checked first: #510 is the absolute, because holding too long costs a
badge and submitting over a person's line costs everything. `deliver_now`'s pre-paste checkpoints
became a bounded re-verify **loop** (`PREPASTE_RECHECK_ROUNDS`) that exits only when both gates
read clear at one instant; each round still contains the full, individually-capped holds, so the
rounds bound *re-arming*, not waiting. Giving up is not a loss — the entry stays at the front of
its queue and the drainer retries with no cap.

Two occupancy checks that did not exist were added. **Pre-Enter:** `wait_for_user_quiet` asks
whether the human *stopped* typing and the question hold asks who owns the Enter key; neither asks
whether human content is in the box *right now*, which is what #510 is about. **At the flush
press:** `should_flush_before_paste_now` enforced #510 only through `human_typed_since`
(`last_user_input_ms > submit_sent_ms`) — a *timestamp*, which is structurally blind to a line the
human typed and left sitting **before** our submit. It now reads `input_pending` directly. That
counter moves only on human writes through `note_user_input`, never on loomux's own `write_bytes`,
so our own stranded paste still does not suppress the flush it exists for.

Declining pre-Enter is not a new recovery path: it takes the same `AbortedPreEnter` route the
question checkpoint beside it already takes — a `StrandedSubmit` marker at the queue front, retried
with no cap, whose press now re-reads occupancy too. The delivery **waits for the box to empty and
then flushes**.

**Be exact about what the pre-Enter gate saves, because it is not the merge.** The merge happens at
the **paste**, not the Enter. If the human starts typing after the pre-paste admission and during
the echo/quiet window, our text is already in their box and nothing here un-merges it. What the
gate prevents is *loomux* submitting that combined line: the human keeps control of when — and
whether — it goes. Their own Enter still submits both, and the queued `StrandedSubmit` marker then
presses Enter on an already-empty box, a harmless no-op that `flush_stranded_text`'s own doc has
always blessed. That sequence is what `the_drain_press_fires_once_the_box_is_empty_again` encodes.
So "never merges into a person's line" would overclaim; "never *submits* over a person's line" is
the guarantee.

**Fix 2 — the hold is bounded, and the bound badges rather than releases.** `QUESTION_HOLD_MAX`
(120s) was never the bound: capping out only aborts *that attempt*, and `run_queue_drainer` re-arms
the same hold every `QUEUE_DRAIN_POLL` forever. Same unbounded-latch shape #518 found in the
human-input block, one guard over. `QUESTION_HOLD_STALE_AFTER` (10 min) bounds the **aggregate,
per-pane** hold, measured off the front queue entry's `enqueued_ms` — the thing the human actually
experienced. `hold_bound_elapsed` is the predicate, and `bound_ms == 0` disables it rather than
firing instantly, so a mis-set constant degrades to silence rather than to a badge on every pane.
What decides *whether* the escalation speaks is the clock and nothing else — see the next
paragraph, which is the whole argument for that and supersedes the `box_pending`-outranks-the-bound
precedence an earlier cut of this design had.

**No signal may veto the escalation.** The first cut of this let `box_pending` suppress the bound,
reasoning that a hold explained by the human's own line needs no report. That repeated this PR's own
bug one level up. `input_pending` is not a reading of the box — it is a counter that only *human*
writes move, zeroed on only `\r`/`\n`, Ctrl-U and Ctrl-C, so a bare `ESC`, a TUI clearing the line
with no occupancy delta, or the CLI consuming the line itself (which loomux never observes) all
leave it stuck above zero over an empty box. With the veto in place, such a pane held forever *and
never told anyone*. An escalation that one of the signals it reports on can silence is not a bound,
so `hold_bound_elapsed` takes the clock and nothing else; the blocked gate decides only which
sentence the human reads (`QuestionStale` vs. the existing `HumanInput` wording), never whether
they hear anything. That also gives the box-occupied hold — previously bounded by nothing — its
first escalation.

**Known limit: the escalation's episode is the queue ENTRY, not the pane's held-ness.** The clock is
`front.enqueued_ms` and the one-shot is keyed on `pty_id`, and those two do not describe the same
thing. *(Fixed in #560 — see the section below. The two consequences are kept here in their
original terms because they are what the next reader will find in the #560 issue and in this
file's history, and because the shape of the mismatch is the argument for the fix.)* Two
consequences, both recorded rather than fixed here (tracked in the follow-up filed
alongside this change):

- *The one-shot resets on any writable poll.* Badge at ten minutes → the gates clear for a single
  poll → `Clear` drops it → the next held poll finds the bound already elapsed and re-badges. Each
  cycle contains a full `deliver_now` attempt, so it is bounded by that function's duration rather
  than by `QUEUE_DRAIN_POLL`, but a human typing on a pane with a delivery queued behind them
  toggles occupancy on every Enter, which is exactly where this PR lives.
- *A stranded marker restarts the clock.* When a delivery pastes and then has its Enter withheld
  (`AbortedPreEnter` — either pre-Enter gate), the drainer pops the text entry and
  `enqueue_stranded_front` pushes a `StrandedSubmit` marker at the **front** with a fresh
  `now_ms()` (`push_stranded_front_locked`). The bound reads `front.enqueued_ms`, so it restarts
  from that moment even though the pane has been blocked continuously. It is bounded — one such
  deferral per *successful paste*, not per poll — but it means the badge measures how long the
  current front entry has waited, not how long the pane has been stuck.

**What canNOT move the clock, stated positively, because it is the reason the bound survives
coalescing.** #533's batching and duplicate-coalescing are unable to advance `front.enqueued_ms`,
and that is structural rather than incidental: `superseded_entries` records the **first** occurrence
of a payload in `seen` and only emits *later* duplicates as superseded, so `entries[0]` is never
itself superseded; `plan_flush` then takes its batch head from `live.first()`, giving
`batch[0] == live[0] == entries[0]`. So no amount of queue churn — duplicates arriving, constituents
being dropped, a backlog being combined into one paste — can hand the bound a younger head entry.
The escalation cannot be starved by a chatty queue, only by the paste/withhold cycle above.

Both known limits are the same question — what ends an episode — and both are caller wiring rather
than a defect in `held_escalation`, whose contract is pinned. The fix for both is a per-pane
hold-start stamp that survives entry churn, which is a change to state ownership and not worth
making inside a review round that has already verified the current shape.

### #560: the escalation clock is the PANE's hold episode, not a queue entry

Both limits above are one question — *what is an episode* — and #560 answers it by making the
episode a thing that exists: `HoldEpisode { started_ms, announced, badged }`, one per pty, held in
`OrchRegistry::hold_episodes`.

**One record, not two flags, and that is the fix rather than a tidy-up.** #532 kept the clock
(`front.enqueued_ms`) and the one-shot (`question_stale_notified: HashSet<u32>`) in two different
places with two different lifetimes, and every symptom followed from letting them disagree. Fusing
them makes the disagreement *unrepresentable*: there is no longer any way to clear the one-shot
without ending the episode, because they are fields of the same value. `announced` (the
once-per-episode `delivery-held-in-queue` line) joins them for the same reason — it used to be
keyed on the drainer's pane-header chip being down, and the chip is LIVE state that comes down on
every writable poll, so that line churned exactly the way the badge did.

**The lifecycle, and what "genuinely releases" means.** `ends_hold_episode`/`opens_hold_episode`
are total over `HoldObservation`, so a future observation cannot be added without deciding in
writing what it does to the clock:

| observation | opens | ends | why |
| --- | --- | --- | --- |
| `HeldPoll` | ✔ | ✖ | the drainer's poll found the pane not writable |
| `Aborted` | ✔ | ✖ | an attempt ran and did not deliver |
| `WritablePoll` | ✖ | ✖ | **provisional** — a reading, not a delivery |
| `Delivered` | ✖ | ✔ | the pane accepted a write; that is the evidence |

Plus one end that does not go through `note_hold`: the pane's queue going away (`commit_exit` for
the drainer's own exits, where the removal has to be atomic with the emptiness check and the
`queue_draining` deregistration; `drop_queue` for the standalone path).

`Aborted` opening an episode is not symmetry for its own sake. A hold can live entirely inside
`deliver_now` — every poll reads writable, every attempt then aborts on a gate that trips during
the paste — and pre-#560 nothing on that path ever started a clock at all. That pane is exactly as
stuck as one that fails at the poll.

**Why a writable poll is not a release, argued rather than asserted.** The drainer does not stop at
a writable poll; it goes straight on to `deliver_now`, which re-reads both gates at the paste and
again at the Enter and can decline at either. So "writable at the instant of one poll" is precisely
the reading a flickering pane produces *between two aborted attempts* — the state #560's first
symptom is reported from, a human typing with a delivery queued behind them. Ending the episode
there fails in both directions at once:

- it drops and re-raises the badge per flicker (`stranded-attention` / `stranded-cleared`), which
  is the audit churn the one-shot exists to prevent;
- and if it also restarted the clock — the obvious "reset on recovery" fix — a pane whose occupancy
  toggles faster than ten minutes would **never badge at all**. That re-creates #532's own bug class
  (an escalation that never fires) via the signals the escalation exists to report on, which is the
  rule `hold_bound_elapsed`'s doc states outright and `.loomux/lessons.md` records as
  "releasing on evidence beats releasing on elapsed time".

This composes with #534's auto-release rather than competing with it. #534 decides *whether the
pane is held* (and its release is itself evidence-based — `QUESTION_RELEASE_CONSECUTIVE_CLEAR_POLLS`
consecutive clear reads of the composed screen). #560 decides *when the clock for a held pane
started*. A #534 release makes the next poll writable, the delivery lands, `Delivered` ends the
episode and drops the badge — the same path a human answering the question takes. Nothing here
re-reads or second-guesses the guard.

**The deliberate cost.** The escalation badge now stays up from the moment it fires until the
delivery lands (or the queue goes away), where before it came down on the next writable poll. On a
pane that is still stuck that is a strict improvement — the old behaviour dropped the badge and
then re-raised it seconds later. The case that genuinely changes is a pane that recovers and whose
delivery then fails for an unrelated reason: the badge persists instead of blinking off. It is
still true (the delivery has not landed), and the alternative is a badge that lies quiet about a
pane nothing is delivering to.

**Restart.** `hold_episodes` is in-memory and owes no `persist_queues` call (#468) — the same
argument its `question_stale_notified` predecessor made, and it is not queue state. A restart
therefore forgets any mid-flight episode: the process that would have badged is gone, `queue.json`
recovery re-admits the payloads (#467), and the fresh drainer opens a new episode on its first
failed observation, so the ten-minute clock restarts from the restart. Persisting it would be worse
in the direction that matters — it would badge on boot for a hold the human may well have resolved
while loomux was down, a false claim on the badge whose whole value is being trustworthy — and the
loss is bounded by the restart announcing itself already, through the recovery notice and
`queue_orphans`.

**Every consumer of the old entry-scoped clock, enumerated** (`enqueued_ms` outside `queue.rs`), and
what happened to each. The enumeration is published including the untouched ones, because "extend
this to every consumer" means naming the list:

| site | what it read `enqueued_ms` for | disposition |
| --- | --- | --- |
| `held_escalation`'s `held_since_ms` | the ten-minute escalation clock | **migrated** to the episode |
| `delivery-held-in-queue`'s `held_ms` | how long the pane has been held | **migrated** (same clock, or the number contradicts the decision beside it) |
| `should_fire_still_queued_notice` | the 30-minute still-queued notice | **migrated**, via `undelivered_since` — see below |
| `coalesced_flush_text`'s per-constituent age | "queued 4m ago" in the flush banner | kept: a fact about that payload |
| `pop_front_dequeued` / `pop_batch_dequeued`'s `queued_ms` | per-entry latency accounting | kept: a fact about that payload |
| `queue_orphans`'s `queued_minutes_ago`, `queue-recovered`/`delivery-recovered`'s `queued_ms` | how old a recovered entry is | kept: a fact about that payload |
| `readmit_recovered`'s `oldest_ms` (the recovery notice's "queued N minutes ago") | how old the recovered backlog is | kept: a fact about those payloads — and it is already a `min` over all of them, never the front |

The rule the split follows: an entry timestamp answers *how long has THIS PAYLOAD waited*, which is
the right question for latency, banners and orphan reports. The episode answers *how long has THIS
PANE been stuck*, which is the right question for anything that escalates to a human.

**The still-queued notice needed both terms, not a swap.** `should_fire_still_queued_notice`'s
parameter was named `oldest_enqueued_ms` and documented as "the FRONT entry's timestamp (the one
that's been waiting longest)" — a claim that is false the moment `enqueue_stranded_front` runs,
since the marker it pushes at the front is the *youngest* entry in the queue. Taking the minimum
over the whole queue fixes that half; it does not fix the case where the marker is the only entry
left, because the batch it stood for was already popped. The episode fixes that case and only that
case — on its own it would lose a backlog that outlives individual successes, since a delivery
landing ends the episode while entries the flush could not fit (`plan.remaining`) stay queued. So
`queue::undelivered_since` takes the earlier of the two, and the parameter is renamed to what it
actually means.

**Testability was part of the defect, not just the fix.** #560's first symptom lived in the
drainer's inline escalation block, and `run_queue_drainer` needs a real `AppHandle` that no headless
test can build — so those lines were unreachable by every test in this repo while `held_escalation`
itself stayed green and pinned through a full review round. Everything that needs no `AppHandle`
moved into `OrchRegistry::hold_escalation_step` (the same argument `flush_stranded_text`'s doc
makes one function over); what is left at the call site is the pane-header chip, which is genuinely
drainer-local state.

One consequence of that split, worth stating because it changes an arm: with the badge one-shot now
surviving a flicker, `HeldEscalation::None` is reachable on a held poll whose chip was lowered by a
`Clear` earlier in the same episode. `None`'s doc says the chip "raised earlier in the episode stays
up", which was true when a `Clear` also ended the episode and is not any more — so the caller raises
the chip on that arm too. `raise_chip` is idempotent per reason, so a steady hold still emits
exactly one event.

**One stated limit.** `badged` records *we raised it*, not *it is still up*. Another mechanism's
`clear_stranded` — the late monitor's `Resolved` arm, `attention_tick` pruning a dead agent — can
take the badge down while the flag stays set, and the escalation will not re-raise until the
episode ends. Deliberately not repaired: a re-raise loop would fight whatever just cleared it, and
the one realistic clearer (`Resolved`) means the delivery resolved, which produces the `Delivered`
observation that ends the episode anyway. The same latch existed before #560 for as long as no
writable poll intervened; what changed is that a writable poll no longer resets it.

**What #560 does not touch.** `held_escalation` itself: its precedence, its one-shot semantics and
its `Clear` arm are unchanged and their tests are untouched. Only the value handed to `held_since_ms`
and the caller's handling of `Clear` changed. `unconfirmed_disposition` and the confirmation
lifecycle are not involved. No queue is mutated by any of this, so no new `persist_queues`
obligation arises under #523's mutation-site table.

**Deferred at the time, landed since:** `push_stranded_front_locked` hardcoded
`reason: EnqueueReason::Question` on every marker it pushed, so the audit trail attributed a
stranded-front marker to a question hold that may not have happened (#560's second issue comment).
It is audit-only — nothing branches on the field — but the record it falsifies is the only one there
is for a write loomux decides to make on its own initiative. The remainder was left out of the
escalation-clock PR because the honest fix is a different review surface, not because it was
acceptable; it is described in its own section below.

### The reason a marker is queued under (#560's residual)

`push_stranded_front_locked` is shared by the two marker pushes, and only ONE of them is a question:

| Call site | Why a marker exists | Reason |
| --- | --- | --- |
| `enqueue_stranded_front` (the drainer, from `DeliverOutcome::AbortedPreEnter`) | a **dialog** owned the Enter key | `EnqueueReason::Question` |
| `enqueue_stranded_front`, same arm | #532's pre-Enter **occupancy** gate declined: a human's own line is in the box | `EnqueueReason::BoxOccupied` |
| `admit_stranded_selfheal` (#496 PR-C) | the pane went **quiet** with our text stranded in its box and loomux decided by itself to press Enter | `EnqueueReason::StrandedSelfHeal` |

Three causes, and the code recorded `question` for all three. They failed in the two ways a shared
seam can: the self-heal's push **hardcoded** a reason it did not have, and the drainer's push
**dropped** one it did (`enqueue_stranded_front` took no `reason` parameter at all, so
`AbortedPreEnter`'s — carried since #532 rev-12 NB1 precisely so it would stop being guessed — died
at the call site).

Both readings are opposites of what was recorded. For the self-heal, "a dialog is on screen" versus
"nothing is on screen at all" — the `delivery-queued` line sent a human reconstructing that wedge
looking for a dialog that never existed. For the occupancy gate the contradiction is inside a single
event: `queued_notice(BoxOccupied)` had already told the orchestrator "pane has human input" while
the audit line for the same delivery, at the same instant, said dialog.

The fix is the same shape at all three: a shared helper cannot know which cause it is serving, so it
no longer guesses. `reason` is a parameter of `push_stranded_front_locked` and of
`enqueue_stranded_front`, and `audit_stranded_push` echoes the caller's rather than re-deciding it —
a second hardcode at the point of RECORDING would put the lie straight back. No new variant was
needed for the occupancy case: `BoxOccupied` is honest for it as it stands, and its doc is amended
to say so (it read "pre-paste … nothing pasted" while #532 had already been handing it to a
pre-Enter abort).

Three consequences worth stating, because they are what made this a separate PR:

- **`queued_notice` matches exhaustively**, so the new variant had to be decided there. It is
  unreachable for a structural reason rather than a convention: the drainer notifies from the reason
  the ABORT carried — a fact about the gate that just declined — never from the entry it is looking
  at, and this reason only ever sits on a marker. The arm is spelled out anyway, which is what has
  kept every notice string in that module honest.
- **`EnqueueReason` is persisted** (it is a field of `QueuedDelivery`, which IS the on-disk record).
  Old file → new build is free: variants are only ever added, so nothing on disk stops parsing. New
  file → OLD build resolves to `parse_snapshot`'s per-entry tolerance — the entry is skipped and
  counted (the caller audits skips), siblings intact. There is deliberately no `#[serde(other)]`
  default: an unknown variant has no safe value, and folding it onto an existing reason would make a
  downgrade silently mislabel the entry, which is the exact defect this variant removes. A visible
  skip beats an invisible lie, and here it costs nothing real — the reason rides only a
  `StrandedSubmit` marker, which no build replays across a restart anyway (`split_recovered`).
- **Still audit-only.** Nothing branches on the value; `cap_headroom` is 0 like every reason but the
  pause-loss notice, and the marker drains through the identical `drain_stranded_submit` press.

**What is still hardcoded, and why it is not this enum's problem.** `run_queue_drainer`'s marker
DRAIN — the arm where `drain_stranded_submit` declines to press Enter — returns
`AbortedPrePaste(Question)`, and that stays. It is not an enqueue reason at all: nothing is being
queued (the marker is already at the front and stays there), and the decline means "a human typed
since our submit, or the ledger already closed", which needs its own vocabulary rather than a reason
borrowed from admission. Recorded here so the next reader finds a decision instead of an oversight.

### Composition with #541's `REINJECT_ACK_SETTLE_MS`, and why nothing here belongs in it

#541 turned that constant into a const expression summing the **five unconditional stages** between
"we decided to paste" and the last Enter, with a standing membership rule: *if you add a stage to
`deliver_prompt` before the last Enter, it belongs in the expression.* #532 adds a pre-Enter gate
(`preenter_admission`) and wraps the pre-paste checkpoints in a recheck loop, so the rule has to be
answered rather than assumed. It has two branches and they need **two different arguments** — the
second is the one a reader will otherwise re-derive from scratch.

- **On the PASS branch, the gate is unconditional but has no duration.** It is a single
  `input_pending` read — no sleep, no poll, no loop. The expression sums *durations*, and this
  contributes none, so it is not a member.
- **On the DECLINE branch there is no Enter at all.** The delivery returns `AbortedPreEnter` and the
  interval the constant bounds never completes, so there is nothing for a floor to be short of.
  That path is not new either: the pre-Enter question checkpoint's `Abort` arm has returned
  `AbortedPreEnter` since #420, long before #532 gave it a second cause. It belongs to the residual
  #541's own doc already disclaims and #546 tracks, not to the sum.

**The boundary is not where it looks, and this is the sentence to keep.** The constant is anchored
at `attempted_ms` — *when loomux decided to paste* — so the pre-paste stages **are inside the
interval**, not outside it. Nothing crosses the unconditional boundary for a subtler reason:
`hold_for_human_input` opens with a guard clause that returns `PasteDecision::Paste { held_ms: 0 }`
when its predicate reads false, *before* the timer starts and before the only `sleep` in the
function. Every checkpoint built on it — both pre-paste holds and all three
`wait_for_question_clear` sites — therefore costs one predicate evaluation and no sleep or poll
window whenever no gate is actually up. That is why `PREPASTE_RECHECK_ROUNDS` can run the pair up to
three times without adding a single unconditional millisecond: it multiplies only the *conditional*
residual, which was already excluded.

So the correct reading is not "these stages don't interact with the floor" — they sit squarely
inside its interval. It is "they contribute nothing unless a gate is up, and when one is, they were
already excluded." Anyone adding a stage here should check which of those two it is: a stage that
sleeps or polls *unconditionally* does belong in #541's expression.

**Two bounds, opposite precedence, and that is deliberate.** #518's `human_input_block` states the
reverse rule — *"`box_pending` outranks the bound"* — and it is correct there. Grep will find both;
they are not a contradiction, and the distinguishing question is **what the bound does when it
fires**. #518's bound *releases a write*, so it must never fire while there is human content to
clobber, and `box_pending` vetoing it is the safety contract. This bound only *raises a badge*, so
there is nothing to clobber and the only failure mode is silence — which is precisely what a stuck
`input_pending` would cause. Same signal, opposite direction, because stuck-true is the safe
direction for a gate that withholds an Enter and the unsafe direction for a gate that withholds a
report. Anyone changing either should check which of the two they are in.

**It raises a badge and never releases a write.** State the limit plainly:
*from an append-only byte ring, a live dialog and an answered one that has not scrolled away are
byte-identical.* No reading available here could justify a release. Narrowing detection to the
last-painted lines *would* discriminate, but it weakens genuine detection — a statusline painted
under a live dialog pushes its y/n line out of a 3-line window, which is precisely why
`prompt_wait_detected` honours structured signals across 12 — and that is the guard's action, which
#427/#420 forbid weakening. The asymmetry settles it: a prompt held too long is recoverable by a
human who is *told*; an Enter that auto-answers a live consent dialog silently steers the agent and
is not recoverable at all. So the bound stops the hold re-arming *silently* and names the
hypothesis; it never converts into a write. The badge wording names **both** branches and gives an
action safe under either ("answer it if one is on screen; if the pane looks clear, that reading is
stale: type a character and delete it") — asserting staleness would be exactly the unbacked claim
`.loomux/lessons.md` calls a defect.

**Open, and deliberately not taken here.** The structural evidence this fix wanted is *rendered
rows*, and #530 has since landed `orchestration::termgrid` — a dependency-free VT replay that
composes the raw ring onto a screen. That is the right foundation for answering "is this question
still **displayed**", which is the one question a byte ring cannot answer.

It is not a drop-in, and the trap is worth writing down for whoever takes the follow-up:
`termgrid::render_screen` returns **scrolled-off history rows followed by the on-screen rows,
joined into one string**. Pointing `prompt_wait_detected` at that output unchanged would reproduce
this exact bug — an answered question that scrolled off is still in the history half, so the
detector would keep matching it, just as it does in the ring today. What the follow-up needs is a
variant exposing **only the on-screen rows** (`Screen`'s `grid`, which the module already keeps
separately from `history` but does not currently return on its own). With that, "the question is
still displayed" becomes a real structural reading rather than a hypothesis, and
`StrandedBlocker::QuestionStale` could become a release instead of a badge — which is the only
thing that would justify one.

*Taken in #534 — see the next section.*

## #534: the question guard reads the composed screen, and a hold can end without a human

**What changed, in one sentence.** `termgrid::render_visible` composes the pane's
**currently-rendered rows and nothing else**, and a hold that the byte ring keeps asserting is
released when that screen shows the question is gone.

`render_visible` is a separate function rather than a flag because of the trap the previous
section names: `render_screen`'s history half would keep matching a scrolled-off question and
reproduce the bug with extra steps. History is not filtered out afterwards, it is **dropped as it
scrolls** (`Screen::keep_history`), so there is nothing left for a caller to reach by accident.
Two exclusions, not one — the *parked primary screen* behind an active alternate screen is
excluded too. `into_text` shows it deliberately (a `get_output` reader wants the context a pager
is covering); it is by definition not displayed, so a "still on screen" reading must not see it.

### What the grid evidence can prove, and what it cannot

It can prove a **negative about the composition**: replay these bytes at this geometry and the
text is not among the cells. It cannot prove a negative about the human's screen, because the
replay begins mid-stream against a blank grid — content painted before the replay window and
never repainted since is absent here and present there.

That gap is closed by *ordering*, not by hoping: the guard replays
`QUESTION_GRID_REPLAY_BYTES` (64 KiB) and the ring detector reads the last
`QUESTION_SCAN_TAIL_BYTES` (4 KiB) **of that same buffer**. Being a strict superset is what makes
the load-bearing claim true — *the paint that caused this hold is inside the window we replayed* —
so a match the ring made and the screen does not hold was overwritten or scrolled, not merely
never seen. Both readings also come from **one** tail read: a second read would sample a different
instant, and the guard would be comparing two screens and calling the difference evidence.

Five things it still cannot see, stated rather than mitigated:

- **A resize racing the read.** Geometry is *required*, never defaulted — no `size()`, no grid
  evidence — because `get_output` can fall back to 80x24 (a wrong width only re-wraps prose a
  human reads) while here a wrong width moves characters between cells and a re-wrapped `(y/n)`
  would read as absent. A resize *during* the window still composes old-width paint at the new
  width. Narrow, transient, and simultaneous with a human's own hand on the window.
- **Escape sequences the replay does not implement.** It is not a terminal emulator (see the
  module's own limits). A CLI placing a dialog with something unhandled composes it wrongly.
- **Wide characters.** One cell here, two in the real pane; a CJK-heavy dialog can shift a row's
  trailing text.
- **A blind-start composition that was never painted over** — which is why a screen with fewer
  than `GRID_MIN_RENDERED_ROWS` non-empty rows is `Unreadable` rather than clear
  (`trustworthy_composition`). "The screen is blank" is not "the screen is clear". This is a floor,
  not a coherence check: a garbled but well-populated composition passes it.
- **A CLI whose resting screen is one line.** The floor above is a row count, and it assumes a
  boxed UI — every CLI loomux drives today paints at least an input box. One that does not
  composes to `Unreadable` forever, so this feature simply never engages for it and the pane keeps
  the pre-#534 badge-only behaviour. Safe direction, and disclosed rather than detected: if a
  Tier-B CLI with a minimal prompt is ever added, this is the assumption to revisit, and the
  fix would be a content-based trust test rather than a taller floor.

### The release rule, exactly

`question_shown` takes both readings. **The ring is the trigger; the grid can only ever release.**

| ring | grid | reading | vs. before #534 |
|---|---|---|---|
| no match | *not consulted* | clear | unchanged |
| match | `Unreadable` | hold | unchanged |
| match | `StillRendered` | hold | unchanged |
| match | `NotRendered` | **clear** | **the one change** |

Every row but the last is prior behaviour, so the entire behavioural surface is **one transition
in one direction**, and that is deliberately all it is. The grid is never allowed to *create* a
hold the ring did not: that would be a new false-positive class (screen content the ring had
already scrolled past — a `❯ npm run dev` in prose the CLI has not scrolled away yet), and while
#420/#427 forbid *weakening* the guard, nothing asks us to strengthen it in the same change that
first lets it release. Strengthening detection with rendered rows remains available and is not
taken here.

`NotRendered` requires **both** sub-readings to come back empty, OR'd toward safety because a
false `NotRendered` is the expensive error and a false `StillRendered` is the cheap one:

- `prompt_wait_detected` over the rendered rows — catches a dialog the CLI repainted with
  different text than the ring matched.
- `match_still_rendered` — looks for *this* match anywhere on screen. Needed because the
  detector's own last-12-lines rule is **chronological**, written for a stream where recent means
  last; applied to a *spatial* layout it would miss a dialog sitting above twelve rows of
  statusline and input box. The comparison is whitespace-flattened, so a line the ring saw as one
  write and the screen wrapped across two rows still counts as displayed.

### The needle is re-read in its own terms, and why that had to be a type

The second sub-reading is only worth anything if it can actually find the thing again. Round 1
gave every match one needle — the matched **line** — and that is unsound for one signal class, in
the false-release direction. Review caught it; it is recorded here because the shape of the
mistake generalizes.

`m.line` comes from `strip_ansi` of the byte ring. `strip_ansi` deletes cursor addressing, so a
CLI that repaints one physical row by cursor address — which `termgrid`'s own header documents as
the flagship CLI's normal behaviour — yields a *concatenation of several frames* that was never on
any screen. Such a needle can never be found on a clean grid.

For the four token-bearing signals that is harmless: the token is short, repaint-stable, and
checked first. For `pointer-option` it was fatal, because that signal's evidence is a **position**
(a glyph leading a line), so it had no token and fell through to the line search. The check
therefore returned false for a menu that was plainly displayed, and since the pointer rule in
`prompt_wait_match` reads only the last three painted lines — chronological again — a live menu
sitting above the input box and status chrome made *both* sub-readings empty. `NotRendered`,
release, Enter into a live menu: the #420 harm, reached by the one class that could not defend
itself.

Two changes, and the second matters more than the first:

- **`pointer_rendered` re-reads the signal itself, spatially.** Every rendered row, deframed by
  the same `leads_with_pointer` rule the ring detector uses — one definition, so a re-read cannot
  drift looser (pinning holds open on prose) or tighter (releasing into a live menu) than the
  detector it stands in for. The line search survives as an *additional* disjunct for both kinds:
  when the line is clean it is the most specific evidence there is, and when it is a repaint
  artifact it simply fails, which can add a hold but never remove one.
- **`QuestionNeedle` is an enum, not an `Option<&str>`.** The round-1 shape encoded "no token" as
  `None` and left what to do about it to a comment — and the comment was wrong. With two named
  kinds, `match_still_rendered` must handle each explicitly and a sixth signal class cannot
  inherit the hole by defaulting. The general lesson: *when a fallback is unsound for one member
  of a set, the set is the wrong type.*

One ordering change follows from the same reasoning: `menu-footer` is now reported **before**
`pointer-option`. Real menus routinely paint both, and the reported match decides which needle the
re-read gets — so among signals that fired on the same dialog, prefer the one whose evidence
survives recomposition. This cannot change the boolean (a disjunction), only which evidence is
recorded and re-read.

`mask_own_paste` runs on **both** readings or the guard contradicts itself: our own just-pasted,
not-yet-submitted text sits in the input box, where it is genuinely *rendered* — and stays
rendered, unlike in the ring, where it scrolls out of a 4 KiB window soon enough. A brief quoting
"do you want to proceed" would otherwise answer "still displayed" about itself and strand its own
delivery.

### Why this is a release and not another badge

The previous section's argument against releasing was specific, not general: *from an append-only
byte ring, a live dialog and an answered one that has not scrolled away are byte-identical. No
reading available here could justify a release.* The reading is now available. The asymmetry it
invoked — a prompt held too long is recoverable by a human who is told; an Enter that auto-answers
a live consent dialog is not recoverable at all — is unchanged and is why the release is fenced
this narrowly.

Four things must all hold before an Enter follows a grid-driven release. They are **not four
independent signals** — saying so would overclaim, and two of them are re-samples of the same
signal class at a later instant. What they defend against is transients, which is a real and
different property from independence:

1. Two **consecutive** clear polls (`QUESTION_RELEASE_CONSECUTIVE_CLEAR_POLLS`), unchanged from
   rev-19 R1. This is what makes ordinary TUI redraw churn safe — but by hysteresis, not by the
   grid: a poll landing *inside* a repaint (erased, chrome painted, dialog row not yet) genuinely
   composes clear, and two such polls in a row would release. The sub-window is narrow (a fully
   erased screen is `Unreadable`, so it must be erased *and* ≥2 rows painted *and* miss the dialog
   row, twice in a row at 250 ms), and `d4b` pins that boundary rather than asserting it away.
2. A composition that passed `trustworthy_composition`.
3. `write_admission` re-reading box occupancy **at the instant of the write** (#532), plus the
   pre-Enter `preenter_admission`. Box occupancy *is* an independent signal here; the question
   half of that same gate is not, because `question_active_now` deliberately reads the grid too
   (see its doc — otherwise it would re-assert, one call later, the hold the guard had just
   released). Its value is the later instant, not a second opinion.
4. The pre-Enter checkpoint re-deriving all of the above after the paste — again a re-sample, and
   again valuable for the same reason: it is taken after a paste that may have taken seconds.

**On "release when the question is gone AND the box is empty".** The box conjunct is not
re-derived inside the question predicate, and that is a decision rather than an omission. It is
already enforced, twice, by construction: the pre-paste loop breaks only on
`write_admission(input_pending, question_active_now)` and the pre-Enter gate is
`preenter_admission`. Re-deriving it here would create a *second* place for `input_pending`'s
precedence to be got wrong — and #536's own finding is that this signal must never be allowed to
veto things it has no standing over, since it latches true over an empty box (a bare `ESC`, a TUI
line-clear, the CLI consuming the line). One derivation, at the point of the write, is the shape
this subsystem already settled on.

**Scope, stated so the boundary is not mistaken for an oversight.** Two other `prompt_wait_detected`
consumers stay on the byte ring: the late-confirmation monitor
(`LATE_MONITOR_QUESTION_SCAN_BYTES`) and the attention scan. Neither decides a *write* — one picks
an audit disposition, the other raises a badge — so neither carries the harm this evidence exists
to bound, and extending them belongs with whoever is measuring those surfaces.

### What the audit now records (#513(c)/F2)

`delivery-aborted-question` recorded `to`/`stage`/`held_ms`/`recheck_round` — everything except
what the guard keyed on. #513's live incident held the orchestrator's inbound queue five times
across 27 minutes and **its trigger is still unknown** because of that. Every question-guard
record (`delivery-aborted-question`, `delivery-held-for-question`, and the retry path's
`submit-retries-skipped`) now carries `matched`:

- `signal` — which detector rule fired (`yes-no-token`, `permission-phrase`, `numbered-menu`,
  `pointer-option`, `menu-footer`). The fastest way to spot a rule misfiring on prose.
- `line` — the normalized line it fired on, capped at `QuestionMatch::MAX_LINE`. Diagnostic only:
  it is a `strip_ansi` artifact and nothing may *decide* on it (see the needle section above).
  Up to 200 normalized characters of pane content therefore reach `audit.jsonl`. That is not a new
  exposure class — the same file already persists the **full text of every delivered prompt**
  (`prompt`, per delivery) and every queued payload — and it is strictly narrower: only
  question-shaped lines, only on a hold or abort, capped, lowercased, local.
- `grid` — `still-rendered` / `not-rendered` / `unreadable`: whether the composed screen agreed.

`matched: null` is a real answer, not a missing field: on an abort record it means the outcome and
the detector disagree, which is itself the finding. `prompt_wait_detected` is now defined as
`prompt_wait_match(..).is_some()`, so there is exactly one detector and the audit cannot describe
a different one than the guard used.

`delivery-aborted-recheck` carries it too — that exit records `blocked_on: "question"` and used to
say nothing about *which*, the same blind spot one exit down. One caveat to read it correctly, and
it is a property of that record rather than of the field: the pre-paste loop runs up to
`PREPASTE_RECHECK_ROUNDS` times, and `matched` there is **the last sighting within the whole
attempt**, which may predate the final round (and may sit beside `blocked_on: "box_occupied"`, the
gate that actually ended it). A later round that saw nothing does not erase it, deliberately: on
this exit the useful fact is that the pane had a dialog during the attempt at all.

## #576: loomux's own notices are not questions

**Problem.** `prompt_wait_detected` asks "does this pane look parked on a question", and loomux's
own notices are text *about* questions. A relayed `report` note or `message_orchestrator` text
lands in a pane as one line:

```
[loomux] w-119 reports blocked: Copilot is asking "Do you want to run npm test? (y/n)" and I cannot answer it
```

That satisfies two **structured** signals at once — `do you want to run` (permission-phrase) and
`(y/n)` (yes-no-token). Structured signals are honored across the last twelve non-empty lines, so
unlike the prose-like ones they are *not* pushed out of range when the CLI redraws its input box
underneath. The gate latches, and a held pane emits no fresh output, so nothing ever pushes the
text back out of the scan window: it does not clear itself. An orchestrator pane is the most
exposed, since relayed worker prose is most of what gets written to it.

The three `question_active_now` call sites all pass `pasted_text: None` — each runs *before* the
entry it is considering has written anything — so `mask_own_paste` has nothing of its own to mask.
The offending text belongs to the **previous** delivery, which nothing was keeping.

**What #534 already covers, and why it cannot cover this.** The rendered-rows reading above is not
re-litigated here: scrolled-off history already cannot satisfy the gate. But #534 lets the *grid*
release a hold the ring is still asserting, and loomux's own text is **genuinely rendered** — both
readings agree it is on screen, and both are right about the pixels. It is simply not a question,
which is a claim about *authorship*, and no reading of the screen can make it.

**The fix.** `mask_loomux_notices` drops rows leading with the `[loomux]` marker. It needs no
per-pane state, which is precisely why it reaches the outer drainer gate that has no `pasted_text`
to work with. Four readers of a live pane apply it:

- both readings inside `question_hold_predicate_sampled` — so every question-gate call site;
- the late-confirmation monitor's own scan, which drives the idle trigger and self-latches
  identically;
- `attention_tick` and `plain_pane_attention`, the two attention-chip readers (#6/#40).

The chip readers were **missed in the first cut and found in review** (rev-126), which is the
useful part of the story. The same notice that latches the gate also satisfies the chip's
"parked on a question" test — and satisfies its quiet precondition *because* a held pane is idle —
so it raises `waiting on a prompt` about a question nobody asked. That is arguably the worse of
the two: the gate's latch is at least escalated at ten minutes by `QuestionStale`, whereas a wrong
chip is reported to nobody and simply trains the human to ignore chips.

The split that allowed the omission is deliberate and kept: `prompt_wait_detected` answers *"is
this question-shaped"*, never *"whose text is this"*, and `mask_own_paste` needs a per-call
argument that signature does not have — so **callers mask, the detector detects**. The cost is
that a new consumer can forget, so `prompt_wait_detected`'s own doc now says so and names this
miss.

### The marker is unforgeable in one direction only

`notify::sanitize_gh_text` rewrites `[`→`(` and `]`→`)` in every untrusted field before it is
formatted into a notice, and `intake`'s own test pins that a third-party issue title can never
produce the marker. So nothing an agent sends **through** loomux can carry it.

An agent's **pane output is not sanitized at all.** An agent can print a marker row itself —
echoing a notice back, quoting one in a summary, or induced to by a hostile prompt. A marker row
is therefore evidence that *someone wrote a notice-shaped row*, never proof that loomux wrote this
one, and the mask is scoped to exactly what that weaker claim supports: **the one row the marker
leads, never the rows around it.**

The rejected alternative is the tempting one. A notice is one logical line (`truncate_notice`
strips control characters, so it cannot contain a newline) and a terminal wraps one logical line
into a contiguous run of non-blank rows — so masking the whole run would also catch a `(y/n)` that
wrapped onto row two. It is rejected because the marker cannot support it: a run-mask hands any
pane the power to delete the rows below an attacker-chosen row, and a genuine permission dialog
painted there would be masked into "no question", releasing an Enter into it. That is the #420
harm, reached from pane output. **Failing open is the dangerous direction**; a hold that should
have cleared is the cheap error, and the ten-minute `QuestionStale` badge already covers it.

`e3` and `e4` in `tests/orchestration.rs` stand guard: a genuine dialog sharing a pane with an
agent-printed marker row still latches, including the no-blank-row shape a run-mask would have
swallowed whole.

### Residual, stated rather than mitigated

- A notice that **wraps** keeps whatever tokens landed past its first row (`e6` pinned this).
- A marker row that has itself **scrolled off** leaves its continuation unmarked.
- A row that both leads with the marker and *is itself* the live question would be masked. This is
  the one-row-wide false-release surface, and it needs a CLI that prefixes its own dialog rows with
  our marker — an agent printing the whole thing has not rendered a dialog, only prose about one.

The first two are under-masks, in the safe direction. Closing them needs loomux to know *what* it
wrote to a pane rather than merely that a row claims it did — a per-pane record of delivered text.
No such record existed when this was written: `DeliveryOutcome` keeps `{confirmed, submit_sent_ms,
from}` and deliberately not the text, and the only copy lived as a stack local in `deliver_now`
that died when the delivery returned. Adding one is **delivery machinery, not detection**, which is
why it became its own change — the next section. The third residual is unchanged and is not
addressed by it.

## #576 residual: masking on what loomux WROTE, not on what a row claims

**Problem.** The two under-masks above have one shape: the marker rule can only ask *"does this row
look like a notice"*, and exactly one row of a wrapped notice does. The continuation rows carry the
detector's tokens (`do you want to run`, `(y/n)` — structured signals, honored across the last
twelve non-empty lines, so a redraw never pushes them out), and the gate parks a pane on a question
nobody asked until `QuestionStale` reports it at ten minutes.

Widening the marker rule is the one thing that cannot be done: an agent can print a marker row
itself, so a run-mask hands any pane the power to delete the rows below an attacker-chosen row —
the #420 harm, reached from pane output (`e3`/`e4`).

**The record.** `OrchRegistry::delivered_notices` is a per-pane, bounded, drop-oldest record of the
**marker-led lines loomux has actually pasted into that pane**, written in `deliver_now`
immediately after `write_bytes` returns `Ok` — the one place loomux's text becomes bytes in a pane.
Three properties do the work:

- **A pane cannot add to it.** The record is written on the delivery side from the text loomux
  pasted, so pane output — the direction the marker *is* forgeable in — cannot reach it. Masking
  keys off the record and never off the marker, which is the #420 rule restated: *an agent-printed
  marker row still widens nothing* (`e14`).
- **Only loomux's own framing goes in.** `loomux_authored_lines` keeps marker-led lines and drops
  everything else, so a kickoff brief, an orchestrator's prompt, and a coalesced flush's verbatim
  constituent payloads never enter it. Recording those would give the mask the power to blind the
  gate to ordinary agent content — the blindness `e11` pins as deliberately *not* taken (`e16`).
- **A producer must opt the notice in.** `mark_notice_maskable` is the door, and it is
  default-closed — see the next subsection, which is the whole of why it exists.
- **Written after the write, never before.** A record of text that never reached the pane would let
  the mask claim rows nobody wrote, which is the fail-open direction.

### The door: marker-led is a necessary condition, never a sufficient one

This was found in review (rev-163), and it is the part of the design most worth reading, because
the first cut got it wrong in a way that read as safe.

A marker-led line is loomux's **framing** — and framing routinely carries a field somebody else
chose. The one that matters: `notify_when(kind, pr, note)` takes an agent-supplied `note`, and
`notify_tick` delivers the fired notice **to the registering agent's own pane**. `sanitize_gh_text`
maps `[`/`]` to parens and strips control characters; it leaves ordinary words, `?`, `(`, `)` and
`❯` alone — everything a dialog row is made of. So with one tool call it makes itself, one agent
can put a line of its own choosing into its own pane's record, print that line's head into its own
pane, and let its CLI paint a permission dialog that is exactly the line's remainder. The run
reconstructs, the dialog rows are masked, the gate reads clear, loomux presses Enter. **One party,
no coincidences** — and the same shape is reachable through a compact re-grounding notice, which
embeds the agent's own directive ledger.

So entry to the record is an explicit promise a producer makes, not a property of the text:

> Every span of this text was either composed by loomux or **called in** by an agent other than the
> pane's own occupant.

`mark_notice_maskable` is where that promise is made and `deliver_relayed_to_orchestrator` is, for
now, the only caller: the `report` and `message_orchestrator` relays, whose fields can be
enumerated and checked (`[loomux] {agent_id} reports {outcome}: {body}` — a loomux-minted id, a
fixed outcome word, and text `from` wrote; no orchestrator-chosen agent NAME, no task title, no
GitHub string). `from == orch` — an orchestrator relaying to itself — takes the default and is not
marked.

**"Called in by" is deliberately weaker than "authored by", and the gap is the accepted residual
below.** What the check can see is which agent made the tool call. Who *dictated the words* is not
visible and arguably not knowable: an orchestrator telling a worker what to report is loomux's
ordinary traffic, with no attack signature to key on. The promise is therefore written as what is
actually enforced rather than as what one would want — an earlier version of this section said
"supplied by", which claimed more than the code delivers.

**Omission is the safe direction.** A producer that never calls it leaves its notices exactly as
they were: the marker rule, one row per marker, a wrapped notice latching until `QuestionStale`
reports it at ten minutes. Forgetting to opt in costs a hold, never a release. That is why the
enumeration above can be short and grow deliberately rather than having to be complete today.

`e14b` pins the closed door behaviourally with the `notify_when` shape; `e18` pins that marking
without a write and writing without a mark both claim nothing; `e20` drives a real MCP `report`
through the relay so the door cannot be perfectly built and connected to nothing.

**What the mask does with it.** `mask_loomux_notices_with_record` keeps the marker rule
unconditionally (one row, no record needed) and adds two rules that are anchored in the record
alone:

- *A wrapped notice.* A run of rows that starts at the START of a recorded line and reconstructs
  that line **to its end**, row by row, is claimed whole. Rows are compared case-folded with
  whitespace runs collapsed, and the boundary between rows may eat one space (word wrap) or none
  (a hard wrap mid-word).
- *A marker that scrolled off.* Every reading is truncated at the **top** (the ring keeps the last
  bytes, the grid the last rows), so the first non-empty row of a reading is the one place a line
  can legitimately appear headless. There, and only there, a run may anchor mid-line — still
  reconstructing to the recorded line's end. A headless fragment anywhere else is not claimed:
  nothing was truncated there, so the rows above it would have shown the head (`e15`). The anchor
  must also clear an evidence floor (`R_TOP_MIN_ANCHOR_CHARS`, 24): without one, a fragment as
  short as a single character would carry a claim over everything below it that completes the
  line, and a genuine scrolled-off continuation is a full terminal row anyway (rev-163 N1).

**Reconstruct-to-the-end is the whole safety argument.** A run that merely *starts* a recorded line
proves nothing about the rows after it, and claiming them would be the rejected run-mask. Requiring
the remainder to be consumed exactly means the last claimed row is the line's last row, so whatever
the pane painted below it is untouched — which is why a live dialog under a **genuine, recorded**
notice still latches (`e13`): the recorded line ends at the marker row, so there is no continuation
to claim. Every failure mode ends the run and keeps the rows: a mismatch, a CLI-truncated echo, a
reading cut short, a line past the record's length cap.

**The width is deliberately not consulted.** Predicting wrap points would need the pane's geometry
*and* the CLI's own re-rendering (prefixes, re-indents, hard vs word wrap), and being wrong about
them fails in the dangerous direction — a wrongly-predicted run would claim a row that is not ours.
Reconstruction asks the safe question instead: not *"where would this have wrapped"* but *"is this
row, verbatim, the next thing we wrote"*.

**Bounds, and why these numbers.** 24 lines per pane, each capped at 512 chars, across at most 64
panes (least-recently-written evicted) — about 786 KiB worst case, stated because these entries are
kilobytes rather than the three fields `last_delivery`'s never-pruned map holds. 512 chars is
`notify::NOTICE_TOTAL_CAP` (400) plus room for a producer's framing; a longer line is truncated,
which costs that one notice its wrap masking and nothing else. 24 lines is bounded by what the mask
could ever *use*: only a line still rendered can be claimed, and `prompt_wait_detected` honors its
structured signals across the last twelve non-empty lines, so a notice that twenty-three later
notices have already pushed past is out of every reading that consults the record.

**In memory only, and deliberately.** The record's single consumer is a mask over a live pane's
rendered tail, so it is worth exactly as long as the rows it explains are still on screen.
Persisting it would put agent-authored note text in a second on-disk place with its own
schema-version burden (`queue.json` is versioned; a new field there is a contract change), to buy
masking for rows a restarted pane has almost always redrawn past. Losing it degrades to exactly the
pre-record behaviour — the marker rule alone, so a wrapped notice latches until `QuestionStale`
surfaces it at ten minutes. That is the cheap error, and `e6` pins both halves: the same fixture
releases with the record and still latches without it.

**Readers.** All of them, together — the two readings inside `question_hold_predicate_sampled` (so
every question-gate call site, including the outer drainer gate and the stranded-submit press), the
late-confirmation monitor's idle scan, and both attention-chip readers. Fixing one reader of this
gate and leaving another is exactly what rev-126 found the first time (`e7`), so the record is
threaded to every one of them in the same change. `question_hold_predicate_sampled` takes it as a
**required** argument rather than a defaulted one: an empty record is a legitimate value (a pane
loomux never wrote to, or one a restart cleared) but it must be stated, not acquired by omission.

**Residual, restated accurately after review.** The record's *content* is partly agent-authored — a
relayed `report` note is the worker's own words inside loomux's framing. So an agent that can (a)
get a marker-led line delivered into a pane, (b) print that line's head into that same pane, and
(c) have its CLI paint a live dialog whose rows are byte-identical to that line's remainder, in
order, hides the dialog.

The first version of this paragraph called that *"two parties and three coincidences"*. **It was
wrong**, and the correction is the reason the door above exists: through `notify_when`'s note, all
three are available to one agent in its own pane, deterministically. The second version said what
remained was a two-party case needing the author to induce the recipient. **That was also too
kind**, for the same reason in a smaller form — and it is worth noticing that this residual has now
been understated twice, which is a fact about how hard the statement is, not just about these two
drafts.

**What actually remains: proxy-authorship by the recipient, and it is orchestrator-shaped.** The
door checks `from != orch`, which is *callership*. An adversarial orchestrator instructs a worker —
*"call `report` with outcome blocked and note exactly `X Do you want to proceed? ❯ 1. Yes 2. No
END`"* — and the worker complies, because complying is its job and not a compromise. The relay is
composed as `[loomux] w-2 reports blocked: X Do you want to proceed? …`, `from` is `w-2`, the check
passes, the line is marked, and it is delivered **into the orchestrator's own pane**. The
orchestrator prints `[loomux] w-2 reports blocked: X` into that pane at will — no inducement, it is
its own pane — and triggers a tool call whose permission dialog paints the remainder. The run
reconstructs, the dialog rows are masked, the gate reads clear, and loomux presses Enter on a
prompt the orchestrator's CLI was asking a *human* to answer.

**The scope is the whole surface.** Both marked call sites target the orchestrator, so every
claimable line in the system is delivered to the pane of the one agent best placed to dictate its
content. This is not an incidental corner of the design; it is where all of it points.

**Accepted knowingly.** Authorship cannot be enforced from inside this mechanism, the two obvious
narrowings are the ones already rejected (vacuous, and gutting), and under-masking remains the
error this whole area prefers. Two levers exist if the cost is judged too high, and both are
subtractions rather than fixes:

- **Drop `R-top`'s mid-line anchor for marked lines**, so a claim must start at a row that is the
  recorded line's own beginning. Shrinks the surface; does not close it; gives up #576's
  scrolled-off half, and since every claimable line is marked, `R-top` becomes dead code.
- **Stop marking altogether.** The pre-record behaviour: the wrap residual stays open for relay
  notices, `QuestionStale` reports the latch at ten minutes, and #576's remaining scope is simply
  not closed.

`e14c` pins the remaining case as known rather than leaving it to be discovered — the `e11`
treatment, applied to the dangerous direction.

Two shapes that were considered and rejected, recorded because they look attractive:

- *Refuse to claim a continuation row that is itself question-shaped.* Vacuous. The rows the record
  exists to claim are precisely the ones carrying `(y/n)` and `do you want to run`; refusing them
  means the gate latches in every case where the record would have changed its answer.
- *Record only notices with no agent-supplied field at all.* Guts the motivating case. #576's own
  example is `[loomux] w-119 reports blocked: <note>`, where the question-shaped tokens **are** the
  note. Masking a relay notice's wrap necessarily means claiming agent-chosen text; the only
  question is whether its author also occupies the pane, which is what the door decides.

## #632: the multi-row notices, fixed at the producer rather than at the mask

**Problem.** `mask_loomux_notices` claims exactly one row per marker (above, and for a reason
this section does not weaken). Two of loomux's notices are **several rows** and **do ride the
pty**, so every row past their first survived into the tail the question gate and the two
attention chips read:

- `pause_suppression_notice` (#615/#569) — an in-band delivery pasted into an orchestrator's
  pane at resume, whose `  - w-2 -> orch-1 (refused, queue full): <preview>` item rows carry up
  to `PAUSE_SUPPRESSION_LIST_MAX` bounded previews of arbitrary agent payloads, plus an elision
  row and a truncation caveat row.
- `queue::coalesced_flush_text` (#533-A) — the flush **is** the prompt, and each constituent is
  introduced by a `----- 3/5 · from w-7 · queued 4m12s ago (id 12, t=…) -----` banner.

Both are more exposed than #624's relay ever was: that block rides an MCP tool result and only
reaches a pane if an orchestrator quotes it back, whereas these two are written to a pane by
loomux itself, every time. This is #576's self-latch, one row further down — loomux's own prose
*about* deliveries satisfying a detector that asks whether the pane is parked on a question.

**The tension, which is the whole of the decision.** The obvious fix is to teach the mask a
bounded block form ("a banner row and everything until the next banner", "a marker row and its
wrap run"). That is precisely the run-mask #621 rejected, and nothing about a second producer
makes it safer: the marker is unforgeable only in the *delivery* direction, an agent's pane
output is not sanitized at all, so **any** rule that lets one row claim the rows beneath it
hands every pane the power to hide a live permission dialog from the gate — and the Enter the
gate then releases answers it. That is the #420 harm reached from pane output, and it is the
failing-**open** direction. The mask's claim is therefore unchanged by this work: still one row,
still leading-marker-only.

**The fix is at the producers, not the reader.** Every row loomux *authors* becomes marker-led,
which is #624's convention applied to the two producers that predate it, and the existing
one-row rule then claims them with no new power:

- the pause notice's item rows become `  • [loomux] w-2 -> orch-1 (…): <preview>`, and its
  elision and caveat rows carry the marker too. The bullet is `•` and not `-` for the same
  reason as #624's relay: `deframe` strips whitespace and `│ ┃ | * ● • ◆`, and **not** `-`, so
  a `  - [loomux] …` row leads with the dash and survives.
- the itemization banner becomes `[loomux] ----- 3/5 · from … -----`. Marker **first**, for the
  same `deframe` reason — `----- [loomux] …` would not mask.

**Why the item rows' previews are masked, though they contain agent text.** The preview is
loomux's own framing *quoting* a payload — `w-2 -> orch-1 (refused, queue full): <preview>` is
the #576 relay shape exactly, text *about* a delivery that is already gone. It is not a rendered
dialog and answering it is meaningless, so it must not park the pane it is being reported into.
The preview is also re-collapsed through `dropped_payload_preview` at render time rather than
trusted: it is read back out of a durable `audit.jsonl` that an **earlier loomux build** may
have written (the legacy-discard cause is that premise, stated), and a preview carrying a
newline would split the item into two rows with only the first marker-led — #632 silently
reintroduced from disk. The re-collapse is idempotent on well-formed input, so nothing changes
for text this build wrote.

**Why the constituent payload rows are NOT masked, and stay able to latch.** A coalesced flush's
constituent text is pushed verbatim and left alone. Three reasons, and the first is the one that
settles it:

1. **It is not distinguishable from ordinary pane content, because it *is* ordinary pane
   content.** The payload is byte-identical to what that same entry would have pasted had it
   flushed alone, pre-#533, with no banner anywhere near it. Masking it would give the coalesced
   path a blindness to question-shaped text that no other delivery path has — a *new* over-mask,
   not the removal of an old one.
2. **The only rules that could reach it are forgeable.** A positional rule (everything after a
   banner) is the run-mask above. A content rule (mask what we recognise as a payload) needs the
   detector to trust bytes an agent chose, which is the hole `LOOMUX_NOTICE_MARKER`'s doc exists
   to name.
3. **The error it leaves is the safe one.** An unmasked payload row means the gate **holds when
   it might have cleared** — a delivery waits, and `QuestionStale` escalates it at ten minutes.
   The alternative error releases an Enter into a live dialog. Under-mask is the conservative
   direction and this is a deliberate under-mask, pinned by `e11` as a documented property
   rather than left to be rediscovered as a bug.

Note also that the delivery path which pastes the flush already strips its own payload rows via
`mask_own_paste` before the pre-Enter check; the readers that see them are the ones with no
`pasted_text` to work with (the outer drainer gate and the chip scans), which is exactly where a
hold-too-long is cheapest.

**Enforced at both ends, because they fail differently** — the #624 pattern, reused. The shared
predicate is `unmaskable_framing_rows(rendered, payloads)`: the rows a rendering leaves unmasked
that are not verbatim payload, written *through* `mask_loomux_notices` itself so it cannot drift
from the rule it stands in for. `pause_suppression_notice` asserts it with an empty payload set
(the whole notice must mask away); `run_queue_drainer` asserts it at the flush call site with
the constituents' text as the exemption, so what is pinned there is the **split** rather than a
blanket claim. Both are `debug_assert` for `park`'s reason: CI's test builds are debug, so a
later edit that adds an unmarked row fails where it is introduced, while a release build never
panics a live session over an outcome whose worst case is a gate that holds too long.

The tests are the other end. `every_row_of_the_pause_resume_notice_is_maskable_including_a_hostile_preview`
puts adversarial previews through the real producer (question tokens, an embedded newline, a
forged marker); `every_framing_row_of_a_coalesced_flush_is_maskable_but_the_payload_is_left_alone`
pins the split in both directions — banners gone, every payload row including a multi-row
constituent's second line still present;
`a_forged_banner_row_buys_an_agent_exactly_one_row_and_never_the_payload_below` is the
adversarial case for the new row shape. `e9`–`e11` put the same producers to the real
question-hold predicate: the resume notice no longer parks its own pane, a genuine dialog
painted directly beneath a fully-masked notice block still latches, and a coalesced payload
still latches by design.

**Residual.** Unchanged from #576's, and this work removes nothing from that list: a notice that
**wraps** still keeps the tokens past its first row (a marker-led row is still one logical line,
and only the first physical row carries the marker), and a marker row that has itself scrolled
off still leaves its continuation unmarked. Both are under-masks. Closing them still needs
loomux to know *what* it wrote to a pane rather than that a row claims it did — delivery
machinery, not detection.

## #727: a pointer glyph with nothing after it is a prompt, not a menu

**Problem.** Deliveries to a pane resumed from a previous session were queued behind "an
interactive question is on screen" and never flushed — 25 minutes over a *visibly idle* pane, three
panes killed, two queued deliveries dropped with them. It reproduced on demand: resume the session,
send anything, watch it queue.

The phantom question was the CLI's own input box. Claude Code paints an empty box as a bare `❯` on
its own row, and `leads_with_pointer` — the `pointer-option` signal — read that as a menu's
highlighted choice. Every `delivery-aborted-question` record in the group's audit log fired on that
glyph, and none on a real dialog.

**Why nothing could clear it.** The glyph poisons both of the guard's readings at once, which is
exactly the case #534's grid release was supposed to cover and the one place it could not:

- On the **ring**, the glyph sits in the last painted lines by construction — it *is* what the CLI
  redraws underneath everything else — so the mechanism that saves the other `❯` false positives
  (finished-turn prose scrolling out of the last-3-lines window) cannot reach it. `strip_ansi`
  deletes cursor addressing, so the glyph also concatenates onto whatever was addressed next,
  producing a matched "line" that was never on any screen.
- On the **grid**, `pointer_rendered` scans every rendered row for a leading pointer and finds the
  same bare glyph. So `grid_evidence_for` answered `StillRendered` for *any* `pointer-option` match
  on *any* screen of that CLI, and the single transition #534 added — match + `NotRendered` →
  release — was unreachable for this signal class. Not weakened: dead.

A resumed pane turns that into a permanent latch rather than a 120-second one. Its restored screen
is static, so no repaint ever comes to change either reading, and the hold ends only when the pane
dies.

**Fix.** The pointer must lead *content*: `leads_with_pointer` now requires something other than
framing after the glyph (`is_frame_char` on both sides, so a boxed empty prompt `│ ❯   │` reads the
same as a bare one). This narrows the **signal**, not the guard — a highlighted menu choice points
*at an option*, and no dialog's selected option is blank — so every hold that ever mattered is
untouched, and the release path that was dead code comes back to life for the class that needs it
most.

**Rejected: widening the notice mask instead.** The ring's matched line was
`❯ [loomux] pr #715 checks: …`, so masking loomux's own notice through a leading prompt glyph would
also have cleared this repro. It fixes the wrong layer: the ring is *allowed* to over-match (it is
the trigger; the grid is what releases), the mask is the security-sensitive surface where a
widening claims rows loomux cannot prove it wrote, and the bare glyph would still have pinned the
grid at `StillRendered` for every other `pointer-option` hold on the same CLI.

**Tests.** `f1`–`f4` in `tests/orchestration.rs`, over
`tests/fixtures/attention/fp-resumed-agent-idle-prompt.txt` — the repro's own restored screen,
captured from `get_output` on the pane that wedged. `f1` pins the glyph inside the detector's
pointer window first, so it cannot pass for the reason the other `❯` negatives pass; `f2` runs the
production predicate over both readings of that screen; `f3` is the fail-safe direction (a pointer
leading a real option still matches, still re-reads on the grid framed or bare, still holds end to
end); `f4` is the latch itself — a genuine menu match on the ring, released once the screen becomes
the idle prompt. The fixture also joins the existing `prompt_wait_detected` and `attention_tick`
negative sets.

**Residual.** A CLI that paints its input box with a *placeholder* (`❯ Try "fix the build"`) still
leads a pointer with content and still matches. That is not new — the older box shape
(`idle-input-box.txt`) uses `>`, which was never a pointer glyph — but it is the shape to watch if
a CLI adopts `❯` plus placeholder text; the answer then is the same one this note takes, a narrower
signal, not a wider mask.

**The assumption this rests on, stated so a future TUI change is a known break and not a mystery:
a pointer and the option it points at render on the SAME row.** That is what makes "nothing after
the glyph" mean "this points at no option" rather than "the option is elsewhere". Two things could
falsify it — a CLI that paints the glyph on its own row above a multi-row option, or a wrap that
lands exactly at the glyph — and either would make a *live* menu read as an empty prompt, which is
the expensive direction. Neither is speculative-only: the whole reason this bug existed is that a
CLI changed its input box from `> ` to a bare `❯`, so the same surface moves again. What limits the
damage if it does is that `pointer-option` is one disjunct of five and the last one tried — a real
dialog painted that way would still have to carry no footer, no numbered option and no yes/no token
to go undetected, and `grid_evidence_for` reads `prompt_wait_detected` over the whole screen as
well. The signal to re-check on a CLI upgrade is therefore this one, and `f3` is the test that
would have to be widened: today it asserts a pointer row and its option are one string.

## #820: a pointer glyph leading loomux's OWN text is not a menu either

**Problem.** On a copilot 1.0.77 fleet, deliveries stranded "quite often" in ordinary use. The
pane-header chip read the question-gate wording — *stuck behind a question the user needs to
answer* — while the screen held nothing but loomux's own prompt, sitting unsubmitted in copilot's
input box. #727 one notch over: that narrowing removed the *empty* prompt glyph, and this is the
same signal, the same latch, with the box **full**.

**Cause: the self-echo exclusion compared the wrong thing.** `mask_own_paste` is what keeps the
detector from reading loomux's own paste as pane content, and its rule was whole-line equality —
`row.trim().to_lowercase() == pasted_line`. A CLI does not render our paste as our lines. Copilot
1.0.7x, specifically:

| What copilot paints | Why equality misses it |
| --- | --- |
| chevron composer input row: `❯ <our line>` | `❯` (U+276F) is a `POINTER_GLYPHS` member and is not framing, so nothing strips it |
| framed composer (1.0.64+ prompt frame): `┃ <our line>`, re-wrapped, `┃ ` on **every** row | each row is a *fragment* of our line, so no row equals one |
| transcript echo of the submitted prompt: `❯ <our line>` + right-aligned dim `HH:mm` | a prefix and a suffix the pasted line does not have |

One surviving row of ours is enough. `leads_with_pointer` needs only a `❯`/`›`/`→` **leading** the
de-framed row: the chevron composer supplies that outright, and in framed mode the `┃ ` de-frames
away and a *wrap boundary* supplies it whenever a brief's own prose puts an arrow at the start of a
row — `red → green`, `main → beta10`, ordinary orchestrator writing. `pointer-option` then fires on
loomux's own prompt.

**Why it latched.** #727's mechanism verbatim. Nothing repaints an unsubmitted box, so the ring's
last-painted window is frozen on the row; `pointer_rendered` scans every rendered row and finds the
same glyph, so `grid_evidence_for` answers `StillRendered` on every poll and `NotRendered` — the
one reading that ends a hold without a human — is unreachable. The capped hold runs out, the
delivery aborts, the text stays stranded in the box, and the queue drainer's own gate
(`question_active_now`, which has **no** `pasted_text` at all) re-reads the same frozen screen
forever.

Claude Code is not hit: its composer prefix is `>`, which was never a pointer glyph.

**Fix: compare against the row the CLI actually paints.** `mask_own_paste` keeps its rule — mask
only what loomux itself pasted — and changes only how a row is *recognised* as ours:

- **de-framed**, via the same `deframe` every other reading here uses, so a bordered composer is
  compared on its content;
- **wrap-reconstructed**, via the same `reconstructs_to_end` discipline
  `mask_loomux_notices_with_record` uses — contiguous, in order, from the line's own start, and to
  its END rather than a prefix, so a run that does not account for the whole line claims nothing;
- **pointer-stripped, as a second attempt only, above an evidence floor**
  (`SELF_ECHO_MIN_POINTER_CHARS`, 24 chars, the `R_TOP_MIN_ANCHOR_CHARS` figure and argument). A
  box border is decoration by construction; a pointer glyph is the entire `pointer-option` signal,
  so stripping one is a claim that has to be paid for. `❯ Overwrite` is our own composer holding a
  one-word paste *and* a live dialog's highlighted choice, byte for byte — claiming it on that
  evidence would mask a genuine dialog into "no question" and release an Enter into it, which is
  the #420 harm.

**#727 said the answer to this shape was "a narrower signal, not a wider mask". This takes the
other one, and the reason is that the evidence available is different.** That residual anticipated
a CLI *placeholder* (`❯ Try "fix the build"`) — decorative hint text loomux does not author, cannot
recognise, and could therefore only ever have handled by narrowing the signal. Here the content
after the glyph is **ours**, known exactly, on the same thread that wrote it. And the mask #727
declined to widen was `mask_loomux_notices`, whose entry is partly agent-authored and which can
claim rows loomux cannot prove it wrote; `mask_own_paste` is not that mask — its input is the
literal byte string `deliver_prompt` just pasted, unreachable from pane output, so the objection
does not transfer. Narrowing the signal is also *exhausted* here: there is no shape separating
`❯ Review requested changes on PR #814…` from copilot's own
`❯ 2. this should reach the agent — please confirm you see this message? (Esc to stop)`, an
arbitrary-prose `ask_user` option. Authorship is the only discriminator left, and it is the one the
mask has.

**Second finding: the hold that strands is the one with no evidence.** Every capped hold and every
abort has carried `matched: {signal, line, grid}` since #513(c)/F2 — but those are bounded by
construction. The hold a human sits in front of lives in the queue drainer, which re-arms the gate
every `QUEUE_DRAIN_POLL` with no cap, and its `delivery-held-in-queue` row recorded
`blocked_on: "question"` and nothing about *which* shape, on *which* line, with what the screen
said. #513's blind spot, one hold class over, and the reason #820 could not be diagnosed from
`audit.jsonl` at all. `question_active_witnessed` now returns the poll's own witness and the
drainer threads it into `hold_escalation_step` — from the same poll as the admission it annotates,
for the same reason `last_user_input_ms` is read there.

**Tests.** `g1`–`g6` in `tests/orchestration.rs`, over two fixtures under
`tests/fixtures/attention/`. Both are **reconstructed from cited copilot-cli sources rather than
captured** — constraint 3 forbids spawning a real copilot — so each test asserts the property it
depends on as a precondition rather than assuming it: `g1` pins the `❯ ` row inside the detector's
own pointer window (the #727 `f1` pattern) *and* that the raw screen genuinely fires
`pointer-option`, `g3` pins that the row which fired is the *wrapped continuation* and not the
first. `g2` is the repro end to end through the production predicate. `g4` pins both halves of the
floor — a short option is not claimable, a long line is. `g5` re-runs every captured real dialog
and every #40/#727 false positive **through the new exclusion**, because the mask is the argument
that changed and a bare detector call no longer covers the risk.

**Residuals, stated.**

- The `question_active_now(…, None, …)` readers have no `pasted_text`, so a paste stranded by some
  *other* cause can still latch them. There are **four** since #819: `deliver_now`'s pre-paste
  re-verify loop, `stranded_marker_action`'s closure (#819's), `flush_stranded_text`, and the queue
  drainer — the last being the one this change witnesses. Closing it generally needs a per-pane record of
  delivered *payload* text, which is exactly the widening `loomux_authored_lines` refuses (`e11`:
  it would hand an agent a row-deleting mask over its own pane). Not taken. The new `matched` field
  is what makes such a hold name itself.

  **#819's reader is the interesting one, and deliberately left alone.** `stranded_marker_action`
  materializes the pane's `stranded_text` a few lines above its `question_active_now` closure, so
  it is the one caller that *could* supply a `pasted_text` without any new record — which makes it
  the obvious place to close the residual, and exactly the wrong place to do it in passing.
  That gate feeds a **blind Enter** (`flush_stranded_text`'s own doc: "a blind Enter with no
  downstream net"), so masking more of the screen there decides when loomux presses Enter into a
  pane it cannot fully read. That is a safety argument about #813/#819's retirement path, not a
  side effect this change is entitled to take, and it belongs in its own issue with its own
  fail-closed reasoning.
- A delivery whose every line is shorter than `SELF_ECHO_MIN_POINTER_CHARS`, sitting in a chevron
  composer, is still unmasked. The floor is what keeps the fix from being a hole; buying the last
  case would need evidence this mask does not have.
- The transcript echo (`❯ <our line>` + `HH:mm`) is not masked — the trailing timestamp defeats
  reconstruct-to-end. It is transient rather than latching: once the ring stops matching,
  `question_shown` is false whatever the grid says, and copilot's own repaints push the row out of
  the last-painted window within a poll or two.
- #727's placeholder residual is untouched. A placeholder is the CLI's text, not ours, so this
  change has nothing to say about it and the answer there is still a narrower signal.

## #821: the same rendering, the other gate — and the failure direction inverts

**Problem.** `box_holds_paste` normalized its tail with `normalize_prompt_text`, which flattens the
whole string. Copilot's framed composer draws `┃ ` down **every** row it wrapped a paste onto, so
the gutter lands *inside* the haystack, interleaved through the needle: the tail normalizes to
`"┃ line one ┃ line two"` against a paste of `"line one line two"`, and containment fails on text
that is plainly still in the box. Same rendering family as #820, one gate over.

**Why it was worse than a wrong answer.** The only thing between a failed containment and a
confident `NotHolding` was a length test:

```rust
if !norm_pasted.is_empty() && normalize_prompt_text(tail).len() < norm_pasted.len() {
    return BoxReading::Unverifiable;
}
BoxReading::NotHolding
```

Decoration **adds** characters, so a tail whose gutters defeated the containment is always *longer*
than the paste it failed to contain. The one safety net is bypassed by the very thing that caused
the miss. A second-order version of the same arithmetic: `box_holds_paste`'s window is
`norm_pasted.len() + BOX_TAIL_WINDOW_SLACK` from the tail end and `Tier1Scan::for_paste` sizes the
read to match, so a paste wrapping past ~100 rows pushes its own start outside the window before
containment even runs. De-framing fixes that too, where widening the slack would not.

**What it costs, post-#819.** #819's licence is explicit that it rests on this reading being true
when it says so — *"Nothing retires on an absence of evidence — the one direction that could
re-open either hazard."* A false `NotHolding` is not an absence of evidence; it is **counterfeit
evidence of an absence**, the one input that argument does not defend against.
`stranded_marker_action` retires on it directly, so the marker retires while our text is still in
the box: the Enter that would have submitted it is never pressed (the strand #813/#819 exist to
repair becomes permanent), the next queue entry pastes on top of it and submits both merged — the
#81/#84/#111 collision #819's own doc names as the hazard this licence closes — and the audit
records `TextGone` for a pane whose text is demonstrably still there. The rule is right; its input
was not trustworthy on copilot's rendering.

**The failure direction inverts, and that changes the fix.** In #820 the gate was the question
detector, where under-masking is safe and over-masking releases an Enter into a live dialog. Here
it is the reverse: a false `Holds`/`Unverifiable` costs a retry (#819 already trades an
unconditional deadlock for a conditional one), while a false `NotHolding` costs the collision
above. **`NotHolding` is the reading that must be earned.** So the fix is two coupled parts.

**1. `normalize_deframed`, on BOTH sides.** `normalize_prompt_text` with each line `deframe`d
first — #820's `deframe`, shared, not a second notion of what decoration is.

Applying it to the tail alone is the tempting version and it is *worse than the bug*.
`is_frame_char` counts `*`, `•`, `●`, `◆` and `|` as framing, so a brief carrying an ordinary
markdown bullet list or table would have those stripped from the tail and kept in the needle. Every
such paste — and orchestrator briefs are full of them — would fail containment, clear the length
guard for exactly the gutter reason, and read as a confident `NotHolding`: this defect
re-introduced by its own fix, on a far more common shape. De-framing both sides cancels it, since
the two lose the same characters and only specificity is spent. The read-SIZING sites
(`tier1_scan_bytes`, `Tier1Scan::for_paste`) keep `normalize_prompt_text` deliberately — they size
a request rather than compare, so an un-de-framed needle only ever asks for more tail, which is
conservative and preserves #559's "no read is ever narrower than it was".

**1b. And the de-framed route is ADDED to the flat one, never substituted for it.**
`box_holds_paste` runs both normalizations and holds if either matches. That is the property the
whole change rests on — **#821 can only ever add `Holds` readings** — and it is structural rather
than argued, which matters because the argued version was wrong twice before it was right (see the
residuals). It also costs almost nothing: the second route runs only on the path that was already
about to answer "not holding".

**2. `paste_echo_probe` — a partial match is never an absence.** De-framing handles the decoration
we know about; the probe covers the decoration we do not, *without modelling any of it*. If the
longest line of our paste is still in the window but the whole will not match, the text is
evidently there and the rendering is what we failed to read — `Unverifiable`, which #819 already
treats as no evidence at all. That is what makes this robust to a trailing gutter, the scrollbar's
own `┃` on the right edge ([copilot-cli#4009](https://github.com/github/copilot-cli/issues/4009)),
a re-indent, or whatever the next TUI release paints.

The probe is taken from **one line**, and that is not an implementation detail. The obvious probe —
"the first N characters of the needle" — spans a row boundary as soon as N passes the first line,
so it dies to the same decoration it exists to detect; a backstop that fails the way the thing it
backstops fails is not one. The longest line, because more of our own prose is stronger evidence;
truncated to `PASTE_ECHO_PROBE_CHARS` (48) so a wrapping line probes only its first row; floored at
`PASTE_ECHO_PROBE_MIN_CHARS` (24, the `R_TOP_MIN_ANCHOR_CHARS` figure and argument) so a paste of
short lines yields no probe rather than a coincidence-prone one.

**Why not `reconstructs_to_end`.** #820 built it, and it is the wrong instrument here.
Reconstruction verifies row *structure* rather than a character sequence — strictly more precise,
strictly more brittle, since any decoration it cannot account for ends the run. In #820 that
brittleness was free: a failed reconstruction under-masks, and an under-mask costs a hold. Here it
would manufacture the confident negative this change exists to remove. **The shared thing is the
definition of decoration (`deframe`), not the instrument built on it** — which is the general
lesson, since the two gates read the same screens through the same CLI rendering and will keep
being tempted toward one recogniser.

**Measurement moved with the comparison — all of it.** `Tier1Scan::read`'s widening and BOTH of
`Tier1ScanCensus::measure`'s terms now normalize the same way. The first cut moved `tail_chars`
alone and left `paste_chars` on `normalize_prompt_text` one line below (rev-305 B1), which
differenced two normalizations: `margin_chars` is a difference, so it is meaningful only while its
two terms measure the same kind of string, and the gap was exactly the framing characters a brief's
own markdown bullets and table rows carry — the `h5` shape, so the bias was systematic,
one-directional, and pointed at over-reporting the very arm the number is read to count. The
read-SIZING sites keep `normalize_prompt_text` deliberately, and the distinction is the whole
lesson: **sizing a request may be conservative, measuring a comparison may not.**

`margin_chars` also now states plainly that #821 added a second `Unverifiable` arm it cannot see —
a read with ample margin can still be unreadable, because the partial-match arm fires on a tail
that is *longer* than the paste — and the census test asserts the surviving **implication**
(negative margin ⇒ `Unverifiable`) with `h3`'s fixture pinned as the counterexample, rather than
the equivalence it used to claim. A doc admission and a test asserting its negation in the same
commit is how the next contributor gets told to "fix" the reading to match the census.

**Tests.** `h1`–`h5` in `tests/orchestration.rs` over two fixtures in the #820 copilot-decorated
family (reconstructed from cited sources, not captured — constraint 3). `h1` is the containment,
with the "tail is longer than the paste" arithmetic asserted as a precondition so it cannot pass
for the wrong reason; `h2` is the *consequence* at `stranded_marker_action`, which is what the bug
actually costs; `h3` is the probe arm, over a fixture whose trailing scrollbar `┃` de-framing
genuinely cannot reach; `h4` is the floor in the expensive direction — a genuinely consumed box
still reads gone and still retires, so the fix cannot buy safety by eroding #813/#819's repair;
`h5` pins the markdown-bullet trap above. #819's retirement matrix and #820's `g1`–`g6` are the
rest of the floor, the latter because `deframe` is now shared across both gates.

**Residuals.**

- The probe can fail, and it is narrower than "a pane under ~48 columns breaks it" (rev-305 Ground
  4 corrected an earlier, more pessimistic wording). The probe is matched against
  `normalize_deframed` of the tail — every row already leading-de-framed and the whole thing
  flattened to single-space-joined words — so spanning a *word-wrapped* boundary is fine, which is
  the entire known copilot shape. It fails only where its span crosses a boundary carrying
  decoration `deframe` cannot reach (a trailing scrollbar `┃`) or a HARD mid-word wrap, which
  splits one word into two tokens and inserts a space the needle lacks. A narrow pane raises the
  odds of crossing a boundary; it is not itself the breaker.

  **Why that is still not a regression, on the argument that survives (rev-306 F1).** An earlier
  wording justified this with "containment is wrap-agnostic", and that clause does not survive the
  second cause: a hard mid-word wrap defeats *containment* too, not merely the probe. The
  conclusion stands on a different footing — that case is **unchanged by #821 rather than
  introduced by it**. `normalize_prompt_text` flattened the break into a space the needle lacked in
  exactly the same way, so a hard mid-word wrap read `NotHolding` before this change and reads
  `NotHolding` after it. What #821 moves is elsewhere, and both moves are away from the dangerous
  reading: de-framing turns the gutter case from `NotHolding` into `Holds`, and the probe turns
  residual failures into `Unverifiable`. Sizing the probe from real terminal geometry would buy
  nothing here and would couple this to a width the comparison deliberately does not know.

  **And the superset property is now structural rather than argued** (rev-306 B1). An earlier
  version of this note disclosed a corner where de-framing could *lose* a match the flat comparison
  had found — a wrap pushing a mid-line `|`/`*` to a row start, so `deframe` strips it from the
  tail (row-leading) while the needle keeps it (mid-line) — and bounded it at "a pane narrow enough
  to wrap a sub-24-character line, degenerate for a GUI pane". **That bound was wrong.** It came
  from the case where no probe is FORMED, and says nothing about whether a formed probe MATCHES:
  the probe samples that same needle line, so it carries the same `|` and fails for the same
  reason. A wrap boundary lands inside its 48-character sample exactly when the composer is under
  48 columns — a 25-47 column four-way split, which is this product's premise, not a degenerate
  case. `ps aux | grep loomux` wrapped before the pipe read `NotHolding` where the flat comparison
  read `Holds`: a new route to the reading this whole change exists to make expensive.

  `box_holds_paste` therefore tries **both** normalizations and holds if either matches, so #821
  can only ever ADD `Holds` readings. That is a property of the construction rather than of a case
  analysis, which matters because the case analysis had already been wrong twice. A narrower
  `deframe` (box-drawing glyphs only, sparing `*` and `|`) would dodge both known shapes, would
  still be a case analysis, and would mint the second notion of decoration that sharing `deframe`
  exists to avoid. The three fixtures now exercise different routes — `h1`'s gutter matches only
  de-framed, `h6`'s pipe wrap only flat, `h3`'s scrollbar neither — which is what keeps both from
  being redundant.

  **The probe is not what guarantees this and never could be**: it shares the failure mode of the
  containment it backstops. Its job is the narrower one of turning a residual containment failure
  into `Unverifiable` rather than a confident absence.
- A wrap landing such that a continuation row *starts* with a character `deframe` strips (the `|`
  in `ps aux | grep`, mid-line in our own text) de-frames on the tail side but not the needle side,
  so the de-framed route fails. **An earlier version of this bullet said the probe catches it as
  `Unverifiable`. It does not** — the probe samples that same needle line and carries the same
  character, so it fails identically; that claim was the reasoning error behind the wrong bound
  above. The flat route is what catches it, and catches it as `Holds`, which is also what the
  pre-#821 code answered. `h6` pins it.
- The premise remains sourced from copilot-cli issue evidence rather than a live capture
  (constraint 3). The fix does not depend on which composer a pane gets: it is about *any* per-row
  decoration, not about `┃`.

## Delivery queue (#445)

**Problem.** `deliver_prompt` has three hold-cap seams — pre-paste box-occupied (#111,
`HUMAN_INPUT_HOLD_MAX` = 60s), pre-paste interactive-question (#420, `QUESTION_HOLD_MAX` = 120s),
and pre-Enter interactive-question (the same guard, run again after the paste) — and until this
PR every one of them DESTROYED the payload once its bounded wait expired: `deliver_prompt`
returned with nothing pasted, the sender was told nothing wrong (the call itself still returned
`Ok`), and — for a non-orchestrator target — the orchestrator got a `[loomux] delivery to <id>
held: ... — re-send when clear` notice whose own wording claimed the opposite of what happened.
Live audit evidence, captured from this repo's own operation and filed on #445: a worker with an
`AskUserQuestion` on screen held two orchestrator prompts for ~120s each, then both were gone —
`delivery-aborted-question` in the log, no trace anywhere else. The user's own words while
waiting: *"I'm waiting for the orchestrator prompts to deliver now to the worker and I'm not
seeing them."* They were waiting for something that no longer existed.

This repo's standing requirement makes the destroy-on-abort design structurally wrong, not merely
unfortunate: the user is frequently AWAY, an agent asks a question, and they answer minutes or
hours later. A hold whose release condition is "a human answers" cannot have a timeout measured in
seconds — but the caps themselves are legitimate: they bound one OS thread BLOCKING per pending
delivery, which is a real resource concern independent of what happens to the payload. The fix
keeps the caps and changes only the outcome at the cap: enqueue, never destroy.

**Approach — a per-pane FIFO plus a drainer thread.** `OrchRegistry` gained
`queues: Arc<Mutex<HashMap<u32, VecDeque<queue::QueuedDelivery>>>>` (keyed by pty id, same
convention as `delivery`/`last_delivery`) and a monotonic `queue_seq: Arc<AtomicU64>` for delivery
ids — plain `std` types, no getrandom (CLAUDE.md constraint 2). `queue.rs` is the pure half
(`QueuedDelivery`, `EnqueueReason`/`DropReason`, `admit()`'s FIFO/cap/coalesce policy, every notice
string, `orphaned_queue_entries`) mirroring `notify.rs`'s own pure-core/impure-wiring split; `mod.rs`
is the impure half.

- **The delivery body was extracted, not reimplemented.** `deliver_prompt`'s ~500-line paste/
  echo/confirm pipeline is now a free function, `deliver_now(...) -> DeliverOutcome`, called from
  exactly one place as of #470 (`run_queue_drainer` — below), for both the fast/uncontended case
  and every replay — the SAME echo retries, submit confirmation, and #451 three-state lifecycle run
  every time, never a parallel reimplementation that could drift. Every parameter is something the
  old closure already captured; nothing about the pipeline's OWN behavior changed by the original
  #445 extraction — see that PR's diff for the byte-for-byte body.
- **`deliver_now` never touches the queue.** On a hold-cap abort it returns
  `DeliverOutcome::AbortedPrePaste(reason)` (nothing pasted) or `DeliverOutcome::AbortedPreEnter`
  (seam 3: the text WAS pasted, only the Enter was withheld — `record_aborted_preenter_outcome`
  still runs, unchanged from #420). As of #470, the entry being attempted is ALWAYS already sitting
  in the queue (admitted before any attempt runs — see "Admission is unified," below), so an abort
  needs no enqueue-on-abort step at all: `run_queue_drainer`'s own match on `DeliverOutcome` just
  leaves the entry at the front (retry next tick) or converts it to a `StrandedSubmit` marker
  in place.
- **Admission is unified (#470 — replaces the pre-#470 "front door" bullet below).**
  `deliver_prompt` admits EVERY delivery into `pty_id`'s queue at arrival
  (`OrchRegistry::enqueue_text`, `reason: Arrival`), atomically with the check for whether the
  queue was empty (`AdmitOutcome::was_first`, decided under ONE lock acquisition, so two racing
  pushes can never both observe "empty"). Only the admission that observes an empty queue is
  responsible for spawning `run_queue_drainer` (`OrchRegistry::ensure_drainer`); every other
  admission — regardless of why it arrived when it did — lands behind whatever's already there and
  best-effort nudges `ensure_drainer` in case a drainer isn't already running (see "Drainer
  lifecycle is atomic," below). This REPLACES a pre-#470 design where an empty-queue delivery
  raced a raw per-pty `Mutex<()>` for the right to attempt a direct paste, entirely separately from
  the queue — see "Ordering," further below, for why that split was an actual ordering bug (not
  merely unfair), and for the historical description of the pre-#470 front door and its
  now-removed B1 pre-paste recheck, kept in `queue.rs`'s `b1_ordering_property` module doc rather
  than here.
- **Single-consumer drain, spawned by whichever admission arrives at an idle queue.** Only
  `run_queue_drainer` ever pops the FRONT of a pane's queue; every admission only ever pushes to
  the BACK. The drainer peeks the front entry, attempts delivery, and pops only AFTER that attempt
  resolves — so the queue stays non-empty for the pane's WHOLE drain, which (post-#470) is what
  makes ordering structurally guaranteed rather than merely likely: nothing can ever observe a
  transiently-empty queue and slip in ahead of an entry still being replayed, because nothing is
  EVER attempted except through this same front-to-back walk. Polls deliverability
  (`!input_pending && !question_active_now`) at `queue::QUEUE_DRAIN_POLL` (2s) with NO cap on every
  pass EXCEPT its first (#470: a freshly spawned drainer's first pass skips both the sleep and the
  deliverability pre-check, calling `deliver_now` immediately — `deliver_now`'s own internal waits
  are the real gate — so the common uncontended case pastes with the same latency the old direct
  path had, WITH TWO NARROW, BENIGN EXCEPTIONS review flagged (N1): (i) if a second entry lands
  within microseconds of the first (before the first's own delivery attempt has resolved), the
  first entry's flush header will claim "N deliveries queued while this pane was blocked" though
  the pane itself was never blocked — `header_pending`'s trigger is queue depth at attempt time,
  not "was THIS entry ever held"; (ii) an entry arriving during the drainer's brief post-pop poll
  sleep is delivered on the NEXT tick rather than instantly, adding up to `QUEUE_DRAIN_POLL` (2s)
  where the pre-#470 direct path would have pasted immediately. Both cost nothing but a slightly
  premature header or a couple of seconds; neither reorders or drops anything). Exits when the
  queue empties, the pty closes, or the agent dies (`drop_queue`/
  `announce_dropped`, below). At most one drainer per pane
  (`queue_draining: Arc<Mutex<HashMap<u32, u64>>>` — a generation map, not a bare membership set;
  see the next bullet for why), spawned on first admission into an idle queue and self-removing on
  exit — the same bounded-thread-lifecycle answer #451's late-confirmation monitors already
  established for this codebase.
- **Drainer lifecycle is atomic with its own exit, and deregistration is generation-owned (#470 B1,
  review rounds 1 and 2).** Round 1: a drainer deciding "nothing left, I'm done" and actually
  deregistering from `queue_draining` used to be two separate steps (peek the queue, see it empty,
  `return` and let the `DrainerGuard`'s `Drop` deregister later) — an OS-scheduling-width window, not
  a rare corner, in which a FRESH admission landing in that gap (`was_first: true`, sender already
  told `Ok`) would call `ensure_drainer`, find the pty STILL registered, and no-op — believing a
  drainer would get to it. That drainer was already committed to exiting and would never look again:
  the delivery sat in the queue forever, never destroyed but never claimed either, with the sender
  already told success. Exactly the "queued means safe" guarantee #445/#451 exist to make structural,
  reintroduced by the PR meant to strengthen it. `OrchRegistry::commit_exit` closed it: the "is the
  queue empty" check and the `queue_draining` deregistration happen in ONE critical section on
  `queues`, so whichever of "a push" or "this exit" the OS schedules first is fully visible to the
  other.
  **Round 2: that fix alone was incomplete — the REAL `DrainerGuard::drop` still ran afterward,
  unconditionally, a second time.** `commit_exit`'s atomicity only covers commit's OWN two steps;
  the guard is a SEPARATE RAII cleanup that fires on every return regardless of whether commit
  already deregistered, because it has no way to know that. A fresh admission spawning a SUCCESSOR
  drainer in the window between commit and the original guard's drop would have its registration
  erased by that stale, unconditional second removal — letting a THIRD arrival spawn a THIRD drainer
  concurrently with the second. Two live drainers on the same queue means the same front entry can be
  independently popped and pasted by each: not a strand (round 1's failure direction) but a
  DUPLICATE — text typed twice into a human's or agent's pane. The fix: `queue_draining` became a
  generation map (`pty_id -> u64`), minted fresh per spawn (`ensure_drainer`'s `drainer_gen`); both
  `commit_exit` and `DrainerGuard::drop` remove `pty_id` ONLY if the currently stored generation is
  still their own. Whichever of them acts first performs the real removal; the other is inherently a
  no-op — structural, not something every call site has to remember to avoid, and this is what folded
  the round-2 N3 finding (`queue_still_notified`'s matching stale-clear) in for free, generation-
  checked in the same step.
  Proven exhaustively — not just at the one reported interleaving, and not just for round 1's failure
  mode — by `queue.rs`'s `unified_admission_property::drainer_lifecycle` module, which models
  MULTIPLE concurrent drainer instances explicitly (registration, processing, pending commit, and the
  guard's own eventual, UNCONDITIONAL drop event — round 2's review finding was precisely that the
  round-1 model omitted this real event) and asserts three properties directly: no admitted delivery
  is ever left unclaimed (round 1), no admitted delivery is ever delivered more than once, and at
  most one drainer instance is ever concurrently processing a given pty (round 2). Mutation-verified
  in both directions per review round 2's explicit ask:
  `non_atomic_commit_still_reproduces_the_round_1_lost_wakeup` (round 1's bug still caught — this
  more-faithful model didn't weaken what round 1 already proved) and
  `unconditional_guard_removal_reproduces_the_round_2_double_drain` (round 2's bug, freshly caught).
- **Every site that mutates `queue_draining`, `queue_still_notified`, `question_stale_notified`, or
  `queues` as a side effect (#470 review round 3, NB-1) — a map of the subsystem, no longer a
  mechanism holding it together.** Produced by grepping every touch point of those four
  fields in `mod.rs` plus every `impl Drop` across the whole `orchestration` module (`DrainerGuard`
  is the ONLY one) — mechanically, not by inspection alone.

  **#562/#497 inverted what this table is FOR, and that inversion is the point rather than a
  footnote.** It used to carry an obligation — *"treat this table as stale the moment a new function
  touches any of the four fields; the fix is to re-run the grep"* — and that obligation failed
  three times in a single day: #533 added two `queues` mutators owing a `persist_queues` call with
  none, and the re-derivation itself dropped a row its own grep HAD returned. A rule enforced by
  remembering to read a document nine thousand lines from the code is not enforced.

  **The two rules this table used to keep are now kept by the compiler** (`src-tauri/src/
  orchestration/queuestate.rs`; see "Compile-time invariants (#562/#497)" below for the mechanism
  and its exact limits):

  - *Every `queues` mutation persists.* `queues` is a `QueueMap`, in its OWN module, with a private
    inner map. The only `&mut` door is `QueueMap::mutate`, which takes the snapshot writer as an
    argument and runs it the moment it releases the lock. A new mutator cannot omit the write,
    because reaching the `&mut` is what schedules it.
  - *Every `queue_draining` removal is generation-checked.* `queue_draining` is a
    `DrainerRegistry` whose only removal is `release(pty_id, generation)`. An ungenerationed
    removal does not compile.

  So a new mutation site added tomorrow no longer needs this table to have been read, and the
  rows below are documentation of the subsystem's shape rather than the mechanism keeping it
  correct. Extending them when you add a site is still worth doing — a reader wants the map — but
  **forgetting to is no longer a durability bug**, which is exactly the difference the two issues
  were filed for.

  **`question_stale_notified` is the fourth field, added by #532**, and widening this table's scope
  to cover it is that change's obligation rather than a later reader's. It is a per-pane one-shot
  flag for the delivery-hold escalation badge, exactly the same class as `queue_still_notified`: an
  in-memory `HashSet<u32>`, drainer-owned, mutated only mid-loop by the sole registered drainer, and
  — the property that matters most here — **not queue state, so it owes no `persist_queues` call**
  (#468).

  **That last classification is checkable, so check it rather than taking it.** The snapshot
  `persist_queues` writes is built entirely by `group_queue_entries`, which reads exactly three
  sources — `self.queues`, `recovered_queue` and `recovered_markers` — and **no notice flag is among
  them**. So neither `queue_still_notified` nor `question_stale_notified` can appear in
  `queue.json`, and mutating one cannot make the file stale. That is a structural argument about
  what the writer reads, not an inference from what these flags are *for*, which is why it survives
  someone later changing what they are for.

  A future change that adds another flag of this kind should add it here too, and should cite the
  same function when classifying it: the table's value is that "what does the drainer touch" has one
  answer, not one per field someone remembered, and its no-persist column is only trustworthy while
  each entry points at the evidence.

  | Site | Mutates | Registration-relevant (`queue_draining`) | Represented in `drainer_lifecycle`? |
  |---|---|---|---|
  | `DrainerGuard::drop` (RAII, every return incl. panic) | `queue_draining` (remove) | **Yes** | **Yes** — `GuardDrops`, unconditional, `guard_checks_generation`-gated removal |
  | `commit_exit` (3 call sites in `run_queue_drainer`) | `queue_draining`, `queues` (remove), `queue_still_notified` (remove), `question_stale_notified` (remove, #532) | **Yes** | **Yes** — the empty-queue branch / `CommitDeregisters` (`atomic_exit`-gated); both one-shot flags' removals are out of the model's scope (same reasoning as the three rows below) |
  | `ensure_drainer` | `queue_draining` (insert, fresh generation) | **Yes** | **Yes** — `Arrival`'s claim-if-unregistered branch |
  | `enqueue_text` (the front door — EVERY delivery's admission) | `queues` (push back, or coalesce-increment an existing entry) | No — never touches `queue_draining`; its `was_first` (computed under the SAME lock as the push) is what *decides* who registers, via `ensure_drainer` | **Yes** — this IS the models' `Arrival` push; pinned directly by `enqueue_text_was_first_is_true_only_for_the_admission_that_finds_an_empty_queue` and by round-1 real-code mutations (`push_front` and `was_first` hardcoded both went red) |
  | `withdraw_unprocessable` (`deliver_prompt`'s no-app-handle rollback — an early-return cleanup path) | `queues` (pop front, conditional on id match) | No — never touches `queue_draining`; only reachable BEFORE a drainer is ever spawned for this admission (no app/registry handle to spawn one with), so nothing it does can race a live drainer's registration | No — out of scope for the same reason: no registration state involved |
  | `enqueue_stranded_front` (seam 3 marker conversion, `run_queue_drainer`'s `AbortedPreEnter` arm) | `queues` (push front) | No — mid-loop, run only by the current sole owner, same as `pop_front_dequeued` below | No — not a registration mutation |
  | `run_queue_drainer`'s `DeliverOutcome::Done` arm | `queue_still_notified` (remove), `question_stale_notified` (remove, #532) | No — mid-loop, not an exit path; only reachable while this thread is still the sole registered drainer | No — deliberately: safe under the (now-correct) at-most-one-drainer invariant; if that invariant were ever broken by a DIFFERENT future bug this could stale-clear a successor's notice flag, but the worst case is one duplicate low-severity notice, not a duplicated paste or a strand |
  | `run_queue_drainer`'s escalation block (#532 — the `held_escalation` match) | `question_stale_notified` (insert on `Badge`, remove on `Clear`) | No — mid-loop, sole-owner, same reasoning as the `Done` arm above | No — the delivery-hold escalation is a badge decision, outside the drainer-lifecycle model's scope, the same way supersession is |
  | `run_queue_drainer`'s still-queued-notice fire | `queue_still_notified` (insert) | No — insertion, not removal | No — not a removal at all |
  | `pop_front_dequeued` | `queues` (pop front) | No — not a registration removal, same mid-loop/sole-owner reasoning | Implicitly yes — this IS the model's non-empty-queue delivery transition |
  | `drop_queue` (standalone — tests, any future non-drainer caller) | `queues` (remove) | No — its own doc states it deliberately never touches `queue_draining`, having no drainer-lifecycle context to stay consistent with | Not applicable — a different code path outside the drainer's own lifecycle |
  | `pop_batch_dequeued` (#533-A, multi-entry branch only) | `queues` (pop front, per constituent) | No — same mid-loop/sole-owner reasoning as `pop_front_dequeued`, which its single-entry branch delegates to | Implicitly yes — N of the model's delivery transitions |
  | `drop_superseded` (#533-A) | `queues` (remove by id anywhere, and mutate a survivor's `coalesced`) | No — mid-plan, run only by the current sole owner | No — supersession is a flush-planning decision, outside the drainer-lifecycle model's scope |
  | `admit_stranded_selfheal` (#496 PR-C — the self-heal gate) | `queues` (push front, via the shared `push_stranded_front_locked`) | No — it *reads* `queue_draining` (nested inside `queues`, matching `commit_exit`'s lock order) to decide whether to decline, but never writes it | No — a declined or admitted self-heal changes no registration state |

  **Fourteen rows, and the arithmetic is stated so a missing one shows up as a mismatch rather than
  as nothing.** Eleven mutate `queue_draining` or `queues`; the remaining three
  (`run_queue_drainer`'s `Done` arm, its still-queued-notice fire, and #532's escalation block)
  mutate only the one-shot notice flags. Of the eleven,
  **three** mutate `queue_draining` itself — the registration state the at-most-one-drainer
  invariant depends on — and all three are represented in the model; **nine** mutate `queues`, and
  every one of those nine calls `persist_queues`, directly or through a caller that does (the
  invariant #468 adds — see "Durability", below). `enqueue_text` is represented in the model anyway
  (it IS the `Arrival` event); the rest are out of its scope for the stated reasons rather than by
  omission.

  **The buckets overlap by exactly one row, and that is why 3 + 9 + 3 is 15 rather than 14.**
  `commit_exit` is the only site in *both* the `queue_draining` and the `queues` bucket, so their
  union is 11, not 12, and `11 + 3 = 14` reconciles. Stated out loud because an unexplained
  off-by-one here is indistinguishable from the defect this arithmetic exists to catch: a reader who
  adds the buckets, gets 15, and "corrects" the row count would be turning a correct table into a
  wrong one. If you change these numbers, re-check the overlap first — a second dual-bucket row
  would make the union 10, and every figure in this paragraph would need to move together.

  **#569 (enqueue-while-paused) adds two CALLERS, no row, and the arithmetic above is unchanged.**
  `deliver_prompt`'s pause branch calls `enqueue_text` (already a row) and `flush_paused_queues`
  calls `ensure_drainer` (already a row); `flush_paused_queues` itself only *reads* `queues` to
  decide which panes to hand to it, and `run_queue_drainer`'s new pause gate `continue`s before any
  mutation on that pass. That is deliberate rather than incidental: routing a pause through the
  ordinary front door is precisely what keeps this table — and #468's every-mutation-owes-a-snapshot
  invariant — true for the pause path without adding a fifteenth thing to reason about. Recorded
  here because "we added no mutation site" is a claim, and this is the table it is a claim about.

  Two of the nine `queues` mutators — `enqueue_stranded_front` and `admit_stranded_selfheal` — push
  through the SAME helper, `push_stranded_front_locked`, which is deliberately not a row of its own:
  it runs with `queues` already held and must not do I/O. Pre-#562 the write was *owed by each
  caller*, which made this the one place in the table where the mutation and its persistence
  obligation lived in different functions — and, not coincidentally, the row the re-derivation
  dropped. It is no longer a split obligation: both callers reach the helper from inside a
  `QueueMap::mutate` closure, so the write follows the mutation whichever function performed it.

  **This table went stale exactly the way its own instruction predicted, and that is worth recording
  rather than just fixing.** #533-A added `pop_batch_dequeued` and `drop_superseded` without adding
  rows here — correctly, because it was written against a base where this table's *other* consumer
  did not yet exist. #468 then made every `queues` mutation owe a `persist_queues` call, so the two
  new sites arrived owing a write nobody knew to give them, and the rebase that brought the two
  changes together merged cleanly with no conflict at either site.
  `the_snapshot_tracks_the_coalesced_flush_paths_too` now pins both new sites against the file
  rather than against the in-memory queue.

  **And then the re-derivation itself dropped a row, which is the part most worth keeping.** The
  grep was re-run as this table instructs, and it *did* report `admit_stranded_selfheal` — the row
  was lost transcribing the output into the table, and the accompanying count ("five sites") had
  already stopped reconciling with anything countable, so nothing flagged the gap. Review caught it.
  Two lessons, both cheap: state the arithmetic (the paragraph above now says thirteen rows, eleven,
  three, nine) so a dropped row shows up as a mismatch instead of as silence; and note that the
  omitted row is one of the two whose mutation happens inside a shared helper rather than in the
  listed function — the exact shape most likely to be skipped by eye.

  Both lessons were about making the manual process *fail loudly instead of silently*, and both
  were the right fix for a table that had to be maintained by hand. #562 was filed the same day
  arguing they were the wrong shape of fix — every previous attempt had been "re-derive it more
  carefully", and the table had by then been wrong in both possible directions. That is the change
  above: the arithmetic and the shared-helper note stay (they still help a reader), but nothing
  durable now depends on either being noticed.
- **Compile-time invariants (#562/#497) — where the two rules above actually live.**
  `src-tauri/src/orchestration/queuestate.rs`, a module whose entire reason to exist is a
  *privacy boundary*: Rust's privacy is per-module and `orchestration/mod.rs` is one 31k-line
  module, so a field declared private there is still reachable from every line of it. Moving the
  two maps into a file of their own is what makes "the only way in is the sanctioned way" a thing
  `rustc` checks rather than a thing this document asserts.

  - `QueueMap::read()` hands out a `QueueRead` that derefs only immutably — deliberately not the
    bare `MutexGuard`, which also derefs mutably and would reopen the door from a method called
    `read`.
  - `QueueMap::mutate(group, writer, f)` is the only `&mut` door. It runs `f` under the lock,
    releases the lock, then calls `writer.write_queue_snapshot(group)` — that ordering is
    load-bearing, not tidiness (`persist_queues` does file I/O and takes `queue_persist`, whose
    order is *before* `queues`, never after; and `enqueue_text`'s ordering argument needs this
    critical section short).
  - `f` returns a `QueueDirty` verdict alongside its own result, so a mutation that genuinely
    changed nothing skips the write **without an escape hatch to skip it with**. There is no
    `discard`/`forget` method, deliberately: #562's own caveat was that a hatch, once present,
    grows one user at a time.
  - `DrainerRegistry` exposes `is_registered`, `claim` and `release(pty_id, generation)` — and
    no unconditional removal at all. The generation check stopped being a rule three call sites
    honoured and became the only removal in existence.

  **The residual, stated because a type can only reject the constructions it can name.**
  `QueueDirty::nothing_persisted()` is a claim the mutation makes about itself, and a wrong one is
  still wrong. Seven of the nine `queues` mutators have a legitimate use for it (a conditional pop
  whose id did not match, a removal that took nothing out, an `entry().or_default()` that interned
  an empty deque `group_queue_entries` will not read). What changed is that a wrong claim is now
  an explicit, greppable line in the diff instead of the silent absence of a call — which is
  exactly the delta that mattered for #533's two sites: both would have had to *state* something
  false rather than say nothing at all.

  **And an honest note about warnings, because #562 proposed one.** The issue asked for a
  `#[must_use]` token making the omission a compiler *warning*. `QueueDirty` carries `#[must_use]`,
  but the enforcement here is deliberately structural rather than lint-based: CI runs `cargo check`
  and `cargo test` without `-D warnings`, and this repo's agent workers are banned from building
  locally at all (CLAUDE.md), so a warning is a diagnostic essentially nobody would ever read. A
  construction that does not compile is the same idea with a mechanism behind it.
- **Seam 3 (stranded pre-Enter text).** Converts to a `QueuedPayload::StrandedSubmit` marker
  (pushed to the FRONT via `enqueue_stranded_front`, ahead of whatever else is queued — it
  represents finishing an already-half-done paste, not a new ask) rather than a text copy. Draining
  it (`drain_stranded_submit`) presses Enter through the EXISTING `flush_stranded_text` logic —
  the same guard a normal delivery's own pre-paste step already uses, so a human's line typed into
  the box in the meantime is never blind-submitted. Single-owner discipline: `last_delivery`'s
  record of this pane is written once, by whichever of {the marker's own flush, the next delivery's
  flush} runs first — unchanged from #420, just now also reachable from the drainer.
- **Bounds.** `QUEUE_MAX_PER_PANE = 8`; on overflow, **reject the newest, never evict the oldest**
  (the head may be the kickoff everything after it depends on). The COMMON overflow case — a
  second-or-later delivery to an already-non-empty queue — is caught at the front door and returns
  a synchronous, truthful `Err` (`queue::queue_full_error`) straight to the ORIGINAL MCP caller
  (`send_prompt`/`report`), so a sender is told plainly rather than led to believe it queued. The
  RARE case — the queue fills DURING a single delivery's own 60–120s hold — can't reach a
  synchronous caller (that thread returned long ago); it's audited (`delivery-dropped`, reason
  `queue-full-at-call`) and drawn as a loud, best-effort orchestrator notice instead. No age-based
  EXPIRY: a queue behind an unanswered question is the designed case for this feature, not a leak,
  so the only age-based behavior is a one-shot, non-destructive `still_queued_notice` at 30 minutes
  (`QUEUE_STILL_QUEUED_NOTICE_AFTER`) making a forgotten blocked pane visible without touching it.
- **Coalescing** is scoped to byte-identical `Text` payloads only (`queue::admit`), never a
  `StrandedSubmit` marker: the queue cannot judge semantic staleness (three DIFFERENT task briefs
  must all deliver — only a literal repeat is provably redundant), so it collapses exact repeats
  and leaves everything else to the sender. Each drain's first delivered `Text` entry carries a
  flush header (`queue::flush_header_text`) reporting how many were queued and how many collapsed.
- **Ordering — closed for real by #470, not merely widened.** As originally shipped, this section
  described a proven gap: arrival order across a whole pane (not just within the queue) held for
  exactly two simultaneously contending deliveries, and was proven NOT to hold for three or more,
  because a fresh, empty-queue delivery raced a raw per-pty `std::sync::Mutex` for the right to
  attempt a direct paste — `std::sync::Mutex` grants to waiters in an unspecified order, so which of
  several simultaneous waiters got served first was independent of arrival order. #470 was filed to
  close that gap and, on review (round 2), was RE-SCOPED before any fix shipped: a reviewer proved,
  using this very exhaustive-interleaving model run under a *simulated fair lock*, that simply
  making the mutex fair is insufficient — the paste-point recheck's own "defer an in-flight
  delivery to the queue's TAIL" step loses that delivery's arrival position whenever a LATER
  arrival used the old front door's separate "queue already non-empty → append directly" path,
  which never touched the mutex, fair or not, at all (NB-A). As filed, #470 would have shipped a
  fair lock, declared ordering solved, and been wrong.
  **What actually closes it: unify admission, don't fair the lock.** `deliver_prompt`'s front door no
  longer has two admission paths to lose position between. EVERY delivery — including ones that end
  up pasting with zero added latency — is pushed onto the SAME `VecDeque`, atomically with the check
  for whether the queue was empty (`enqueue_text`'s `was_first`, decided under one lock acquisition
  that can never observe two different answers for two racing pushes). Only the push that observes
  an empty queue is responsible for starting `run_queue_drainer` (now the ONLY caller of
  `deliver_now`, for both the fast/uncontended case and every replay); every other push, regardless
  of why it arrived when it did, is already correctly positioned behind whatever got there first.
  There is no more "defer to tail" step for a position to be lost at — a delivery's place in the
  queue is fixed the instant it's admitted and never moves until it's delivered — and the raw
  per-pty `Mutex<()>` `deliver_now` still locks is now redundant defense-in-depth (only one thread
  is ever running `deliver_now` for a given pty at a time, enforced by `queue_draining`), not the
  ordering mechanism. `queue.rs`'s `unified_admission_property` module proves this exhaustively at
  three, four, and five simultaneous contenders (no ceiling — unlike the old algorithm, FIFO-by-
  construction doesn't get harder at higher N) and is mutation-verified (a one-line "pop from the
  back instead of the front" mutation reliably produces violations the same checker catches). The
  formerly-open residual test — `b1_ordering_property::pre_470_algorithm_three_way_contention_
  inverted_arrival_order`, renamed by review (B3) from `..._can_still_invert_..._known_residual`:
  a PASSING test whose name claims an inversion "can still" happen is itself a claim-vs-reality
  hazard once #470 ships the fix that closes it, readable out of context by `cargo test` output, CI
  logs, or a grep, with no doc in view to correct it — is kept UNMODIFIED otherwise (it still
  correctly proves the OLD, now-replaced algorithm has the gap) rather than deleted or silently
  repurposed — see that module's doc for why, and `unified_admission_property::
  unified_admission_closes_ordering_at_three_contenders` for the same property, same contender
  count, now proven closed under the new mechanism.
  **Liveness, not just ordering (#470 B1, review round 1):** unifying admission closes ordering but
  initially reopened a DIFFERENT defect — a lost-wakeup race in the drainer's own exit, where a
  fresh admission's `ensure_drainer` call could no-op against a drainer that had already decided to
  exit but not yet deregistered, silently stranding a delivery the sender was told succeeded. See
  "Drainer lifecycle is atomic with its own exit," above, and `unified_admission_property::
  drainer_lifecycle` for the exhaustive liveness proof (`OrchRegistry::commit_exit`).

**Notice vocabulary — part of the fix.** The old text ("held: pane has human input — re-send when
clear") is deleted along with `paste_held_notice`/`question_held_notice`/`held_delivery_notice`/
`should_notify_paste_held`/the old `notify_delivery_held` method — replaced by `queue::queued_notice`
("queued ... — delivers automatically once clear; do NOT re-send"), sent through the new
`notify_queue` (same suppression discipline as every delivery notice: never to an
orchestrator-target pane, never to a paused group) via `deliver_prompt`'s abort-handling
continuation. `orchestrator.md`'s held-delivery guidance was rewritten to match: never re-send on a
`queued` notice; only a `DROPPED` notice means the payload is actually gone.

**Every suppression now leaves a trace.** The routed #451 finding this issue also owns — review
found the late-correction notice (`notify_delivery_confirmed_late`) left no audit trace when its
own orchestrator-target/paused-group suppression fired, structurally the same silent-suppression
class as this issue's core defect. `notify_unconfirmed_delivery`, `notify_delivery_confirmed_late`,
and `notify_queue` all now write a `notice-suppressed` audit line (`kind`, `to`, `reason`) on every
suppression branch, so a suppressed notification is discoverable after the fact instead of just
vanishing. Since #578 `notify_queue`'s orchestrator-target line additionally carries `parked: true`
and the notice `text`, which is what makes that line a record of the notice rather than a record
that one existed — see "The orchestrator's notice inbox" below.

**Persistence: the intake gap, first stated honestly (#445) and now closed (#468/#467).** The plan
#445 implements originally defended its in-memory choice partly by saying a restart's loss is
"mechanically derivable from `audit.jsonl`, and the orchestrator's session-start re-sync is the
documented recovery." Intake caught that the SECOND clause was false: the orchestrator's documented
re-sync (`list_tasks`, `get_state`, `list_agents`, `list_notifications`, an issue scan) does not
scan the audit for orphaned queue entries, and no tool surfaced that view — so the claimed
mitigation was aspirational, not real, the exact claim-vs-reality failure that cost #451 four review
rounds. #445 resolved that by making the FIRST clause genuinely true and filing the second as
separable work: `queue::orphaned_queue_entries` (pure) and `OrchRegistry::queue_orphans` (the impure
wrapper reading a group's real `audit.jsonl`) scan for `delivery-queued` ids with no matching
`delivery-dequeued`/`delivery-dropped`, but nothing called them from anywhere the orchestrator reads,
so the loss was forensically visible and not recoverable. That is what #468 (durability) and #467
(wiring) close, together, below.

### Durability (#468/#467)

**The queue is written to disk on every mutation.** Each group's `queue.json` (beside `state.json`,
`tasks.json` and `audit.jsonl` in the group dir, because it is the same kind of thing) holds exactly
what is queued for that group right now, written through `atomic_write` — same-directory temp,
`sync_all`, rename — which is the #133 precedent, and the reason a crash or a full disk leaves the
previous good snapshot rather than a truncated file.

**A snapshot, not a journal — the one real design choice here.** #468's filing named "an on-disk
`queue.jsonl`" and both the #133 (atomic whole-file replace) and #240 (atomic per-record append)
precedents. Append-only is what `audit.jsonl` uses and it is the right shape for an event history;
it is the wrong shape for this. A journal of queue events would need replay logic to reconstruct the
current state, compaction to stay bounded, and would leave a half-replayed queue reachable if a
record were ever lost — three failure modes bought for nothing, since a pane's queue is capped at
`QUEUE_MAX_PER_PANE` (8) and the whole file is therefore a few KB. So: whole-file, atomic, rewritten
per mutation. The audit log remains the append-only history of what happened; `queue.json` is only
ever what is pending.

Writing is serialized by `OrchRegistry::queue_persist`, held across "read the live queues → write the
file" so a stale snapshot cannot land after a fresh one (a mutation completing after some writer's
read must wait on the same lock and then re-read). **Lock order is `queue_persist` before `queues`,
never the reverse**, and `persist_queues` is only ever called with no lock held: doing the I/O under
`queues` would stall every delivery in the registry and lengthen exactly the critical section #470's
ordering argument depends on staying short.

**What a restart can rebind, and why neither obvious key works.** #468's filing proposed rebinding by
`agent_id`, on the stated grounds that it is "durable across a restore" — this is false, and finding
that out is most of why the mechanism looks the way it does. Agent ids are minted off an in-memory
counter (`orch-{seq}` / `w-{seq}`) that restarts with the process, so after a restart they *repeat*:
the first worker spawned in the new process is `w-1` whether or not it is the same worker. Keying
recovery on `agent_id` would hand one agent's backlog to an unrelated pane that merely inherited its
number. `pty_id` is no better — the terminal layer re-mints those too, which #445 already knew.
`a_queued_delivery_survives_a_restart_...` pins the id collision directly, so the day someone
"simplifies" `rebinds_to` back to an id comparison, a test says why not.

Two identities do survive, and both are stamped onto every entry at admission (`DurableTarget`):

- **`to_orchestrator`** — a group has exactly one orchestrator, so "was this addressed to the
  orchestrator" outlives the name it was addressed by. This is the live-incident path: worker
  reports and loomux notices are queued to the orchestrator pane.
- **`session_id`** — the agent CLI's conversation id. `spawn_agent(resume_session, cwd)` reopens
  exactly that session, so a resumed worker is the same worker in the only sense a pending delivery
  cares about.

`QueuedDelivery` also carries `group`, because the live queue map is keyed by pty id and pty ids are
registry-*global* (groups share one `OrchRegistry`) — without the stamp, a per-group snapshot would
have to re-derive each pane's group from the agents map, and an entry whose agent was already reaped
would silently fall out of every group's file.

And `delivery_kind` (#620), which is persisted because every field here is but is **not** a restart
field: it exists for a delivery held across a *pause* inside one process, where the resume needs to
know the brief was a kickoff (see the pause section above). A restart deliberately drops it —
`readmit_recovered` re-admits through plain `enqueue_text`, i.e. as `MidSession` — which is also what
`#[serde(default)]` gives an entry written before the field existed.

**Recovery: re-admit what binds, surface the rest, never drop silently.** `recover_persisted_queue`
reads a group's snapshot back exactly once per process (lazily, on first touch by a bind or a
`queue_orphans` call — there is no single "a group was restored" callback in this registry, and
`recovered_groups` is what makes running twice impossible rather than merely unlikely). Entries land
in `recovered_queue`, a staging area deliberately separate from the live `queues` map: every one of
them was queued for a pty that no longer exists. They leave it exactly two ways.

- **Re-admitted** (`readmit_recovered`, called from every bind site) when the pane that just bound
  matches by `queue::rebinds_to`. Re-admission goes through `enqueue_text` — the same front door
  every other delivery uses — so recovered entries take their place by the ordinary admission rules
  rather than being spliced in beside them, and #470's ordering argument covers them unchanged. It
  runs BEFORE that pane's own kickoff is delivered, because an entry queued before the restart
  arrived before the kickoff and admission order is delivery order.
- **Surfaced** by `queue_orphans` otherwise — with the payload, which is the whole point: the
  orchestrator re-sends rather than reconstructs. This is the common case for workers, since loomux's
  own restore kickoff already tells the orchestrator its worker panes are gone.

**`queue_orphans` merges two derivations** (`queue::merge_orphans`): the snapshot (payload included)
and #445's audit scan (no payload, but needs no snapshot, so it still covers a group whose entries
were queued by a build older than this one). Snapshot wins on a shared id. `delivery-recovered` is
in the audit scan's terminal set, but is written only when an entry actually LEAVES staging (a fresh
`delivery-queued` id now tracks the payload) — see hazard 3 below for why closing it any earlier
silently blinded the derivation that needs no snapshot. Until an entry leaves staging both views
report it, which is what the merge exists to dedupe.

**A defect this wiring exposed in the pre-existing derivation.** "Queued, never resolved" is
satisfied by *every entry currently in the queue* — an undelivered entry has not been audited as
delivered yet either. That was harmless while nothing called `queue_orphans`; it is not harmless now
that a tool tells the orchestrator "a non-empty result is lost work, re-send it", which would have
turned the recovery feature into a duplicate-delivery generator. `merge_orphans` takes the set of
live ids and excludes them. (`queue_orphans_finds_an_enqueued_entry_with_no_terminal_event`, written
under #445, simulated its "restart" inside one process and so asserted the looser meaning; it now
performs a real relaunch and additionally pins that a live entry is never reported.)

**Five ordering hazards the lazy read creates, and how each is closed** (three found in
self-review, two more in review round 1 — the pattern is worth naming: every one of them is
"something observed the group between the restart and the end of recovery"). Making recovery lazy is
what avoids inventing a "group restored" callback this registry does not have, but laziness means
the *first* thing to touch a group after a restart decides what happens — and it is not necessarily
a bind.

1. **Never overwrite a snapshot nobody has read.** An admission rewrites `queue.json`. If that ran
   before the read, queueing one new prompt would destroy the previous process's entire backlog,
   silently, with nothing having looked at it. `persist_queues` therefore runs recovery first — the
   read is hung off the WRITE, so the ordering holds by construction rather than by every call site
   remembering it. It is called *before* `queue_persist` is acquired, because recovery can emit a
   notice, a notice is a delivery, a delivery persists, and `std::sync::Mutex` is not reentrant.
2. **Never re-mint an id the snapshot still holds.** `queue_seq` is in-memory and restarts at zero,
   so a fresh admission would be handed id 1 — an id a recovered entry already carries. Both
   consumers of an id break silently on that collision: `queue_orphans`'s live-id filter hides a
   real orphan behind an unrelated fresh delivery that reused its number, and the audit scan lets
   the new id's `delivery-dequeued` close out the old id's `delivery-queued`. Recovery seeds
   `queue_seq` past the snapshot's high-water mark, and every id-minting path (`enqueue_text`,
   `enqueue_stranded_front`, `admit_stranded_selfheal`) runs recovery before it mints. Uniqueness is
   guaranteed only *within* a group, which is all that is needed: every consumer of an id — the live
   filter, the audit scan, the snapshot — is group-scoped.
3. **The snapshot carries staged entries, not just live ones — and an id is closed in the audit
   only when it leaves staging.** These are one fix because the two halves are **complementary**,
   each closing a different view (review round 1, finding 1). Measured, not assumed — the isolated
   mutation runs on #523 show that neither half alone produces the loss: reverting only the
   snapshot half removes the payloads while the audit derivation still names the ids, and reverting
   only the audit half closes that derivation while the snapshot still carries the payloads
   (`a_second_restart_still_reports_a_backlog_nobody_had_read_yet` passes under that one). Only
   together is the entry gone from both views.
   **An earlier revision of this paragraph said "either alone still loses work"; that was an
   overclaim and the runs disproved it.** It is corrected here rather than deleted because this
   paragraph is the thing a future change will trust instead of re-deriving, and knowing that the
   two halves are complementary rather than individually sufficient is exactly what would let
   someone remove the "wrong" one. Writing live queues only meant the first admission after a restart rewrote
   the file without the backlog it had just staged into memory; and because `delivery-recovered`
   was written at *stage* time, the audit derivation — the one view needing no snapshot — was
   already closed. A process dying before the orchestrator's session-start `queue_orphans` call
   therefore lost the backlog from *both* views, permanently and silently, which is strictly worse
   than not persisting at all (pre-#468 the audit scan would at least have kept naming the ids).
   Restarts cluster, and the exposed set was exactly the case the feature exists for. Now: staged
   entries and markers are written on every snapshot, so recovery is re-runnable across any number
   of restarts; and `delivery-recovered` is written by `readmit_recovered` at the moment a fresh
   `delivery-queued` id starts tracking the payload. Until then both views report the entry and
   `merge_orphans` dedupes them. A `StrandedSubmit` marker never leaves staging, so it audits as
   the non-terminal `queue-stranded-unreplayable` rather than `delivery-dropped` — it must never
   stop being reported.
4. **The once-only guard is held across the whole critical phase, not just the check.** It used to
   be published in a temporary guard released before the file read, the `queue_seq` seed and the
   staging insert, so a second thread arriving in that window saw "already recovered", returned,
   and minted an id the snapshot still held — silently re-opening hazard 2 (review round 1,
   finding 2). MCP dispatch is thread-per-request and the moment after a restart is precisely when
   every agent in a group reports at once, so this is the feature's own acceptance scenario, not a
   corner. Holding `recovered_groups` across phase 1 makes a concurrent first touch *wait* for the
   seed, and closes the same window for `readmit_recovered` (which would otherwise see empty
   staging mid-recovery and let a kickoff precede the backlog). The constraint this creates, and
   the reason notices are collected and sent in a phase 2 after the guard drops: **nothing inside
   phase 1 may deliver, enqueue, or otherwise re-enter recovery** — `std::sync::Mutex` is not
   reentrant, so a same-thread re-entry deadlocks rather than looping. Auditing is safe (file I/O,
   no callback); a notice is not, since a notice is a delivery, a delivery enqueues, and an enqueue
   persists.
5. **A re-admission that fails goes back to staging.** Re-admission is capped like any other
   (`QUEUE_MAX_PER_PANE`), and a recovered backlog can meet a pane that already has live traffic in
   it. A rejected entry returns to `recovered_queue` and keeps being reported by `queue_orphans`
   (audited as `delivery-requeue-rejected`). Dropping it there would be a *worse* silent loss than
   the bug this feature fixes, because the sender was told long ago that it was safely queued.

**Limits, stated rather than argued away.**

- **A `StrandedSubmit` marker is never replayed.** Its whole meaning is a live pty's input-box
  contents — "the text is already pasted, press Enter" — which a restart destroys. Replaying it would
  press Enter on whatever the *new* session has in its box, most likely a human's half-typed line.
  Markers are never re-admitted, audited individually as `queue-stranded-unreplayable` (reason
  `queue::STRANDED_ORPHAN_REASON` — the same single string the orphan row and the MCP tool
  description use, so grepping the audit for what the tool just showed you finds it), and announced
  through `stranded_lost_notice`, which says plainly that this text is NOT recoverable: unlike a
  `Text` entry there are no bytes left to re-send, only the `prompt` audit line to read. They are
  also reported by `queue_orphans` with `text: null` — the notice is best-effort (recovery can run
  before any orchestrator pane is bound) and "never silently dropped" cannot rest on a delivery
  that is allowed not to happen.
- **A one-entry window during re-admission** (review round 2 N4; tightened after review round 3).
  `readmit_recovered` takes one staged entry at a time, and on **either** outcome the entry is back
  in a durable store before the next one is taken: a successful `enqueue_text` makes it live (and
  persists), and a rejection at the pane's cap puts it straight back into staging and rewrites the
  snapshot immediately, rather than parking it until the loop ends. So at any instant every
  recovered payload is in staging, or live, or is the single entry in flight between them. A crash
  in that gap drops that ONE entry from the snapshot — not silently: no `delivery-recovered` was
  written for it, so the audit derivation still reports it, payload-less.

  Two earlier shapes were wider, and the second is worth recording because the code looked fixed
  and the note said it was. Round 2's original shape partitioned the whole matching set out of
  staging up front. The round-2 *fix* narrowed the success path but left rejections parked in a
  local vector for the rest of the loop — so a recovered backlog meeting a pane already at
  `QUEUE_MAX_PER_PANE` (hazard 5's own case) still had every matching entry outside both stores at
  once, while this note claimed one. Review round 3 caught the discrepancy between the note and the
  code; the code is what changed. One entry is the irreducible width, for the same reason the next
  bullet exists — two durable stores cannot be updated in one atomic step. The cost of holding it
  there is one extra snapshot write per rejection, on a path that only runs when a full pane meets
  a recovered backlog.
- **One double-delivery window remains.** A crash between the drainer's pop and the snapshot rewrite
  replays an entry that already landed. It is bounded — by the queue's existing byte-identical
  coalesce, which collapses the replay into whatever is queued, and by `pop_front_dequeued`
  persisting before it audits — but it is not closed, and pretending otherwise would be the exact
  claim-vs-reality failure this section's own history is about.
- **An unknown snapshot `version` recovers nothing** rather than guessing at the shape; the failure
  direction of guessing is pasting the wrong bytes into somebody's terminal. A single unreadable
  entry costs only that entry, and the skip is audited (`queue-recover-skipped`) rather than leaving
  the count silently short.
- **Interaction with the coalesced flush (#533-A), checked rather than assumed.** The two features
  meet at three points and hold at all three. (i) Admission-time coalescing (`queue::admit`, exact
  byte equality) is untouched by #533, so the bound on the double-delivery window below — a replayed
  entry collapses into an identical live one — still holds as written. (ii) Supersession is a
  *drain-time* decision (`plan_flush`, called from the drainer alone) over the live queue; it never
  reads or writes `recovered_queue`/`recovered_markers`, so it cannot race the staging hand-off.
  (iii) `drop_superseded` moves a folded entry's coalesce count onto its survivor, which is state a
  recovered flush header reports — so it persists, as does `pop_batch_dequeued`'s multi-entry branch,
  which pops without delegating to `pop_front_dequeued`. Neither inherited a write when the two
  changes were merged; both are in the mutation-site table above and pinned by
  `the_snapshot_tracks_the_coalesced_flush_paths_too`.
- **Staged orphans are never cleared.** Once staged, an entry keeps being reported by
  `queue_orphans` for the life of the install — it may MOVE off the hot snapshot into the archive
  (#547, below), but nothing stops reporting it. There is no "acknowledge" step, deliberately:
  nothing in the registry can tell whether the orchestrator actually re-sent the work or merely read
  the row, so an ack would be a claim the code cannot back. The cost is that an orchestrator calling
  `queue_orphans` twice in one session sees the same rows twice, which is why the tool's own
  description says to call it once at session start (a restart is the only thing that produces
  these, so re-polling finds nothing new). The alternative failure — forgetting lost work — is the
  one this feature exists to prevent.
- **No age cutoff.** #468 asked whether staleness should expire a recovered entry; it does not — the
  same answer #445 gave for the live queue (a queue behind an unanswered question is the designed
  case, not a leak). Instead the staleness *judgement* is handed to whoever can make it:
  `recovered_notice` says how long the backlog waited, and `queue_orphans` reports
  `queued_minutes_ago` per entry.

### Bounding the snapshot write: the staged-orphan archive (#547)

**The cost the two bullets above create.** `queue.json` is rewritten and fsynced on every
admission, and it carries the staged set as well as the live queues. Staging is never cleared, and
worker panes do not survive a restart, so every restart permanently adds that restart's unbindable
backlog: the per-delivery write grows with the age of the install rather than with what is pending.
Staged payloads are uncapped on disk too — `ORPHAN_TEXT_CAP_BYTES` clamps what `queue_orphans`
hands back, not what is stored. The "a pane's queue is 8 entries, so the file is a few KB" argument
that justified whole-file rewriting stopped covering the file once #523 put staging in it.

**The fix is a change of FILE, not of disposition.** Staged entries that no longer belong on the
hot path move into an append-only `queue-orphans-archive.jsonl` beside `queue.json`. A cap and an
audited eviction were both considered and both rejected in #547's own filing: the staged set exists
to report work nobody received, so dropping from it reintroduces one level down the silent loss
#523 exists to remove, and an eviction the orchestrator only learns about from an audit line is
still a drop at the moment it matters. So nothing is dropped. Everything rolled is still on disk,
still surfaced by `queue_orphans` (with `source: "archive"`), still re-admitted by
`readmit_archived` when its pane binds, and named per entry in the audit
(`queue-orphan-archived`, carrying id, target, reason, staged age and payload size).

**The policy is three rules, in order** (`queue::plan_archive`, pure over an id/age/bytes
projection so the decision costs no clone on the hot path):

1. **Age** — staged longer than `STAGED_ARCHIVE_AFTER_MS` (24h). The rebind window is minutes, not
   days: a pane rebinds in the restore that follows the restart.
2. **Entry backstop** — `STAGED_HOT_MAX_ENTRIES` (64 = eight full panes), oldest id first.
3. **Byte backstop** — `STAGED_HOT_MAX_BYTES` (64 KiB of payload), oldest id first.

The two backstops ignore age deliberately. A crash loop stages a fresh backlog every few seconds
and every entry in it is younger than the window, so a bound with an "unless it is recent"
exception is not a bound. Rule 3 exists because rule 2 does not bound the WRITE: a queued payload
is a whole task brief. This is not the age cutoff #468 refused — that would have expired a
recovered entry; this expires nothing, and the staleness judgement stays with the reader
(`queued_minutes_ago` survives the move unchanged).

**Ordering, in both directions, is append-then-remove.**

- Rolling off: the archive append is fsynced (`append_durable`) and staging is edited only once it
  returns `Ok`. A crash or a failed append leaves the entry in BOTH stores, never in neither;
  `queue::parse_archive` dedupes on delivery id and `queue_orphans` skips an archived id that is
  still staged, so the duplicate is invisible and self-healing. A failed append audits
  `queue-archive-failed` and the snapshot simply stays fat — a cost, not a loss.
- Re-admitting: `enqueue_text` persists the fresh live entry before the archived line is removed,
  so the same crash leaves a duplicate rather than a hole. Its cost is the double-delivery window
  #467 already bounds — the replay is byte-identical, so `queue::admit`'s exact-equality coalesce
  collapses it into the live entry. The rewrite re-reads the file under `queue_persist` rather than
  writing back what it read minutes earlier, because `archive_staged_overflow` may have appended in
  between and a stale rewrite would delete those appends.

**The rewrite works over RAW LINES, never over parsed records** (review B1), and that is the
difference between "nothing is dropped" being a property of the file and being a property of the
reader's vocabulary. `parse_archive` is deliberately lossy — it skips a line whose version this
build does not know, because interpreting one would mean guessing at a shape — which is right for
every path that ACTS on records (the orphan report, the rebind) and fatal for the one that
REWRITES the file: rebuilding it from parsed output deletes exactly the lines the per-line `v`
exists to protect. The failure it produced is worth stating, because it is the shape this whole
section argues against arriving through the mechanism meant to prevent it: a newer build writes
`v: 2` records, a rollback follows, and the next bind that re-admits anything erases them —
silently, since the skip count was discarded. So `queue::scan_archive` reads the file as lines,
each carried through byte for byte, and parsing answers one question only: *is this the record I am
deliberately removing?* A line that is not JSON at all is carried too — unreadable is not the same
as unwanted, and a line with no readable id can never be matched, so it can never be removed.

The **`queue_seq` seed reads ids the same raw way**, for the same reason and against the same
failure: an id sitting on a line this build cannot interpret is still an id in use, and reading it
through `parse_archive` would make a newer build's record invisible to the very check that exists
to stop an id being re-minted. The one case that genuinely cannot be accounted for — a line whose
id is unreadable even as raw JSON, so there is no number to account — is audited
(`queue-archive-id-unreadable`) rather than left as a silent shortfall, because the consequence of
a re-minted id is itself silent.

**Archived entries re-bind BEFORE staged ones.** An entry only reaches the archive by being among
the oldest, ids are monotonic per group, and admission order is delivery order — so re-admitting
the archive after staging would invert arrival order across the two stores, which is the property
the N5 id-sort exists to protect.

**The `queue_seq` seed reads the archive too**, and that is load-bearing rather than tidy.
Archiving takes ids OUT of the snapshot, so a group whose whole staged set has rolled off presents
an empty file; seeding from it alone would restart the counter at zero and re-mint an id an
archived orphan still carries. Both consequences of that collision are silent, and they are the two
the seed was written for: `queue_orphans`'s live-id filter hides a real orphan behind an unrelated
fresh delivery, and the audit scan reads the new id's `delivery-dequeued` as closing the old id's
`delivery-queued`. One extra file read, once per group per process, inside the recovery guard.

**Compatibility, both directions, on `QueuedDelivery`'s serde precedent.** Each archive line
carries its own `v` (per line, not per file — an append-only file outlives the build that wrote its
older records), `#[serde(default)]`ed so a line written before the field existed still parses, and
`parse_archive` skips-and-counts a line whose version this build does not know rather than guessing
at its shape. Downgrading past the archive entirely loses SIGHT of archived entries — such a build
never opens the file — but destroys nothing: `queue.json` is still a valid, smaller snapshot of the
same schema, and the audit-derived orphan view still names the ids.

**Archive-era skew — the case the per-line `v` actually exists for — is the one where an older
build DOES open the file**, and it is handled structurally by the raw-line rule above rather than
by that build's ability to read what it finds. Skipping a record it cannot interpret and
*preserving* that record are different obligations; honoring only the first is what review B1
caught. Both are honored now, so "nothing is dropped" does not depend on the reader understanding
every record — the only version of that claim worth making about a file designed to outlive the
build that wrote it.

**What was deliberately NOT done here.** `queue_orphans`'s orphan LIST stays uncapped. It was
already unbounded before this change (staging accumulated the same way), so capping it is not a
regression this PR creates, and a cap on that list is a change to the tool's wire shape — the
`refused` list's own cap (#579) is the precedent for that being owed its own argument, and the
argument would be about an orchestrator's context budget rather than about disk writes.

**Relationship to #451 — clean composition by construction.** #451 owns everything AFTER Enter
(acceptance evidence, `Confirmed`/`Pending`/`Failed`, the late-correction notice); this queue owns
everything BEFORE paste (plus the seam-3 stranded-Enter marker). A drained delivery is a fresh
`deliver_now` call that enters #451's lifecycle exactly like a direct one — `resolve_submit_
confirmation`, the Pending monitors, and `should_notify_unconfirmed`'s internals are untouched by
this PR. The abort call sites #451 deliberately left alone (so its own diff stayed clean) are
exactly the lines this PR edits, which is why this PR was sequenced to land after #451 merged.

**A deliberate simplification against the plan's original framing.** The plan describes the flush
header as "the first thing a drain delivers" — implying its own delivery attempt. This PR instead
PREPENDS the header to the first replayed `Text` payload's own paste (one paste, header then
content) rather than sending it as a separate delivery with its own echo/confirm cycle: materially
the same information reaches the pane, at a fraction of the implementation risk and one fewer
`deliver_now` call per drain. The tradeoff: a queue whose FIRST entry is a `StrandedSubmit` marker
(seam 3 fired on a delivery to a previously-empty queue) drains with no header — there's no fresh
paste to prepend it to, and injecting one would corrupt the stranded text's own Enter timing. Rare
(it requires seam 3 firing before any OTHER entry existed) and not silent — every entry still gets
its own `delivery-dequeued` audit line — but noted here as a known gap, not asserted away.

**What cannot be tested here, and what closes the gap.** Exactly the residual #451's own design
note names for the same reason: `deliver_prompt`'s thread body requires a live Tauri `AppHandle`
and a real pty (`tauri::test`'s `MockRuntime` isn't the concrete `Wry` runtime this code needs),
and CLAUDE.md constraint 3 forbids spawning a real agent CLI to get one. So `run_queue_drainer` and
`deliver_now`'s full pipeline are NOT exercised by the test suite — what IS covered, and to what
depth: `queue.rs`'s pure policy (admit/cap/coalesce/notice text/orphan-scan) unit-tested in
isolation, including two exhaustive-interleaving models — `b1_ordering_property` (kept as a
historical record of the pre-#470 mutex-race algorithm and its proven two-contender fix /
three-contender residual) and `unified_admission_property` (#470's replacement mechanism: ordering
proven mutation-verified at three, four, and five contenders, and — its nested `drainer_lifecycle`
submodule, modeling multiple concurrent drainer instances explicitly — liveness (no admitted
delivery ever left unclaimed, round 1), uniqueness (no admitted delivery ever delivered more than
once, round 2), and mutual exclusion (at most one drainer instance ever concurrently processing a
pty, round 2) all proven mutation-verified in both directions — see "Ordering" and "Drainer
lifecycle," above); the registry-level bookkeeping (`enqueue_text`, `enqueue_stranded_front`,
`pop_front_dequeued`, `drop_queue`, `queue_orphans`, the front-door check reaching `deliver_prompt`
with no app handle at all) integration-tested end to end against a real `OrchRegistry` and a real
`audit.jsonl`; the notice-suppression discipline (`notify_queue`) tested directly. The drainer's
LIVE wiring — a real pane actually flushing on unblock, and the pre-paste recheck actually firing
inside a real `deliver_now` call — is a hand-validation item, the same bar #451's own non-firing
tier was caught by:

1. Block a worker pane with an `AskUserQuestion`; send 3 prompts (one a byte-identical duplicate)
   → held badge, then `delivery-queued` x2 + `delivery-coalesced` x1; answer after several minutes
   → flush header, both deliver in order, `delivery-dequeued` x2, zero duplicates.
2. Same via a human line left sitting in the box (box-occupied path, #111).
3. The original incident replayed: a worker `report(...)` while the ORCHESTRATOR holds a question;
   answer much later → the report arrives with the flush header; nothing destroyed.
4. A 9th prompt to a pane already blocked with 8 queued → synchronous, loud rejection at the MCP
   caller (`queue_full_error`), never a silent drop.
5. Kill an agent holding a non-empty queue → `dropped_notice` + one `delivery-dropped` audit line
   per entry.
6. Restart loomux with a non-empty queue → v1 loses it; verify `queue_orphans` on the surviving
   `audit.jsonl` reports exactly the queued-without-terminal entries.
7. **rev-35 B1's own scenario, live:** block a worker pane; send a prompt (D1, holds); ~30s later,
   BEFORE D1's hold times out, send a second prompt (D2) — verify D2 also holds (not an instant
   paste) rather than racing D1. Answer the question only after D1's hold has timed out and
   enqueued. Verify D1 delivers before D2 (`delivery-dequeued` for D1's id precedes D2's), never the
   inverted order the review reported.
8. **#470's three-contender scenario, live:** from three different senders (e.g. two workers and
   loomux itself), fire three prompts at the SAME blocked pane within the same few-hundred-ms
   window, in a known order D1/D2/D3. Verify `delivery-queued` audit lines land with ids in the
   SAME order the sends were issued (not merely that all three eventually land), and that draining
   after the block clears produces `delivery-dequeued` in that same D1/D2/D3 order.
9. **#470 B1's lost-wakeup scenario, live:** send a single prompt to an idle pane and let it drain
   to completion normally (drainer exits, `queue_draining` empties). Immediately — within the same
   poll tick, i.e. as fast as `gh`/the MCP client allows — send a second, unrelated prompt to the
   SAME pane. Verify it delivers (not stranded): a `delivery-queued` line followed, within a few
   seconds, by `delivery-dequeued` for the same id, never a `delivery-queued` with no matching
   terminal event while the pane sits idle and answerable. This is the live analogue of
   `unified_admission_property::drainer_lifecycle`'s exhaustive proof — worth confirming once
   against real thread scheduling, not just the model.

**Follow-ups filed at PR time, not built here:** wiring `queue_orphans` into the orchestrator's
session-start re-sync (the persistence-gap closer, above, #467); on-disk queue durability (#468);
a PR-C queued-count pane badge on the existing `orch-delivery-held` event seam.

**#470 — the three-plus-simultaneous-contenders ordering residual, since closed.** Discovered as a
mutex-fairness gap while building this PR's exhaustive-interleaving test, then RE-SCOPED on review
(round 2) once a second, independent mechanism — defer-to-tail position loss (NB-A) — was proven to
survive a fair lock. Closed by unifying admission rather than by widening the fair-lock fix; see
"Ordering", above, for the full argument and `queue.rs`'s `unified_admission_property` module for
the exhaustive proof.

### Front-door refusals (#579)

**The one loss with no queue entry to its name.** Every mechanism above is keyed on a delivery
id, because every mechanism above is about a payload that WAS queued. A delivery refused at the
cap never was: `enqueue_text`'s `RejectFull` arm returns before `queue_seq.fetch_add`, so no id is
minted and the payload cannot join `orphaned_queue_entries`'s open/close map, `merge_orphans`'s
dedupe, or the live-id filter. It is not that `queue_orphans` under-reported refusals — it was
structurally incapable of seeing one. #563 met the same problem from the audit side and split this
out rather than folding it into #572's visibility fix, precisely because closing it means touching
the `queue_orphans` wire shape, and that is a public-contract change owed its own argument.

**The contract choice: a second list.** Three shapes were available and two are worse.

- *A synthetic id* — mint a number for a refusal so it can ride in the existing `orphans` array.
  Rejected: `id` is not decoration, it is the join key three separate consumers run on, and a
  number that joins against nothing while looking like one that does is the "a claim is a
  deliverable" defect in wire form. It would also collide with `queue_seq`'s real ids or need a
  disjoint range nothing else knows about.
- *`Option<u64>`* — widen `OrphanedQueueEntry.id`. Rejected: every existing row's id becomes
  nullable, for the benefit of rows that can never have one, and every downstream reader has to
  learn a `None` case that means "this row is a different kind of thing." If the row is a
  different kind of thing, say so in the shape.
- *A second list* — `refused`, beside `orphans`, keyed on `{from, to, preview}`, the same naming
  #569's `SuppressedDelivery` settled on for the same reason. Chosen. `count`/`orphans` keep their
  exact pre-#579 bytes, so the change is additive for every existing reader, and the new keys are
  always present (`refused_count`, `refused_omitted`, `refused`) so a reader never has to
  distinguish "no refusals" from "this build does not report them".

The decisive argument is behavioral, not aesthetic. An orphan is a payload loomux still **holds** —
staged in `recovered_queue`, re-admitted the instant its pane rebinds. A refusal was **declined**,
and the sender told so synchronously. Putting refusals in the orphan list would put them within
reach of `readmit_recovered`, and silently re-admitting an hour-old declined delivery would reorder
it against everything the pane accepted in the meantime — the third acceptance criterion #579 filed.
Being audit-derived and read-only, `front_door_refusals` cannot do that even by accident, and
`reporting_a_refusal_never_re_admits_it` pins it because "read-only" is the kind of property a
later refactor spends without noticing.

**Recovering the payload — verified, not assumed.** #572 made the refusal line identifiable
(`from`, `bytes`, a bounded `preview`); #579's acceptance asks for enough to *re-send*, and a
160-char preview is not that. The bytes do survive: `deliver_prompt` audits `prompt` with the FULL
text immediately before it admits, on both the paused and unpaused paths. So a refusal is paired
with the most recent `prompt` line from the same sender to the same target, and that line is
accepted only if **both** of its fingerprints match what the refusal itself recorded —
`text.len() == bytes`, and `queue::dropped_payload_preview(text) == preview`, recomputed. Two checks
rather than positional adjacency, because audit writes from concurrent delivery threads interleave
and "the line just before" is not a guarantee. On any mismatch the row reports `text: null` and the
reader falls back to the preview, which is the safe direction: the failure mode of guessing here is
handing an orchestrator the wrong bytes to paste into somebody's terminal.

The residual, stated: a second `prompt` line for the same (sender, target) pair, written by another
thread inside the window between this delivery's own `prompt` line and its refusal, whose text also
agrees on total byte length *and* on its whitespace-collapsed first `DROPPED_PREVIEW_MAX` chars.
That is either the same payload or a near-twin of it; it is not closed, and
`a_refusal_reports_no_text_rather_than_bytes_it_cannot_verify` pins the length-only case a weaker
join would have accepted.

**Three things the list deliberately does not do.**

- **It does not double-report.** A recovery re-admission refused at the cap (`readmit_recovered`,
  `EnqueueReason::Recovered`, hazard 5 above) writes the same `queue-full-at-call` line, but that
  entry goes back into staging and keeps being reported as an *orphan* — with its payload, by an
  id. Refusals carrying that reason are excluded, or one lost payload would appear twice in one
  tool result whose documented response is "re-send" (`a_recovery_re_admission_refused_at_the_cap_
  is_reported_once_as_an_orphan`).
- **It does not claim to be complete — at either of the TWO caps it sits under.** Unlike orphans,
  which `QUEUE_MAX_PER_PANE` already bounds at 8 per pane, refusals accumulate without limit — a
  pane held at capacity refuses every arrival for as long as it stays there. So the *list* is capped
  at `REFUSED_LIST_MAX` (8, borrowing `PAUSE_SUPPRESSION_LIST_MAX`'s argument for the analogous
  cost: not a paste into a pane this time, but a read into an orchestrator's context, at up to
  `ORPHAN_TEXT_CAP_BYTES` per row). The most recent survive, since a pane at capacity keeps
  refusing; `refused_count` states the true total and `refused_omitted` what the list cap left in
  `audit.jsonl`.

  **The second cap is the one the first version of this bullet missed** (review NB1), and it is
  worth recording because the bullet was already *about* not doing this. The scan reads
  `audit_log`, which keeps only the most recent `AUDIT_VIEW_LIMIT` (5000) entries — so `total` was
  never a count of a group's refusals, only of those in the readable tail, and `refused_omitted`
  measured the list cap against a window that had itself been cut. The composition is what makes it
  a defect rather than a hedge: a busy group whose refusals all sit in the older half reports
  `refused_count: 0, refused_omitted: 0` — the *strongest* claim this shape can make, "nothing was
  ever refused", asserted by a scan that never saw the evidence. That is the silent truncation
  `.loomux/lessons.md` names, produced inside the bullet promising not to produce it, which is how
  a no-silent-caps rule fails in practice: the author bounds the cap they happen to be thinking
  about.

  Closed with `refused_window_truncated`, reported by `audit_log_windowed` — the reader that
  performs the cut, and therefore the only place that knows it happened. It is a **required
  parameter** of `front_door_refusals`, not a field filled in afterwards, so a future call site
  cannot omit it and quietly restore the complete-looking count. Deliberately NOT inferred
  downstream from `entries.len() == AUDIT_VIEW_LIMIT`: a log holding exactly the cap was not
  truncated, and a derivation that called that "truncated" would cry wolf on a boundary it has no
  way to resolve. Same job as #569's `PauseSuppression::window_start_seen` — a scan that ran off the
  start of its own timeline has to say so — and the same lineage; the only difference is that an
  exact signal is available here (`a_truncated_audit_window_is_reported_as_truncated_not_as_complete`).
- **It does not pretend a refusal is restart-shaped.** The orphan list is produced by exactly one
  thing, which is why its advice is "call it once at session start." Refusals happen in ordinary
  operation, so that list can be non-empty on a session with no restart in it — and every one of
  them was already reported to its sender in-band. The tool description and `orchestrator.md` both
  say so, and say which rows actually need action: those whose sender has since died, and those
  where `from` is `loomux` itself, because then nobody was listening.

**Two shapes, because `queue-full-at-call` is written twice.** `enqueue_text`'s arm refuses a text
payload (`payload: "prompt"`, recoverable bytes). `audit_stranded_push`'s refuses a `StrandedSubmit`
marker (`payload: "stranded-submit"`), which never had text at all — the bytes were already pasted
into the pane and only the Enter was queued, so that pane is sitting with an unsubmitted prompt in
its box. "Re-send it" is the wrong instruction there, so the row carries the audit line's own
`consequence` string verbatim rather than a second wording of it, and points the reader at the pane.
Reporting only the prompt case would have made the list's own claim false, which is the failure mode
a partial list presented as complete always is.

**What this list did NOT cover as #630 shipped it — enumerated then, closed by #633.** The claim
`refused` made was exactly "every writer of `delivery-dropped` with `reason:
"queue-full-at-call"`", which is the two sites above — *not* "every way a delivery can fail to
arrive with nobody told." Three paths were invisible to every derivation, and naming them was what
kept that claim from quietly widening into the "surfaced, never silently dropped" overclaim #445's
own history is about:

- `deliver_prompt`'s **`agent … is dead`** and **`agent has no terminal yet`** refusals, which wrote
  no audit line at all — so nothing downstream could surface them, however good the derivation. The
  second is #615's own residue: it turned a silent `Ok` into a silent `Err`.
- `withdraw_unprocessable`'s **`no-app-handle`** drop, which *is* audited — and was therefore the
  one audited drop reason this scan's `reason == "queue-full-at-call"` filter passed over in
  silence, on the argument that it is unreachable in production (`self.app` is set once at startup).

**"Every refusal, not just the capped one (#633)", below, closes all three** — each refusal now
writes its own reason and the filter enumerates them — so the honest statement of what an
orchestrator gets from `refused` is no longer "every delivery refused **at the queue cap**" but
every delivery `deliver_prompt` refused for any reason, bounded by the two caps above. Read that
subsection for the reason set and for why the no-app-handle drop was decided IN rather than left
excluded.

**#578/#624's parked orchestrator notices are OUT, decided rather than inherited.** #624 landed a
notice channel for the orchestrator's own pane while this was in review: a queue notice whose target
IS the group's orchestrator cannot be typed into that pane (it would queue behind the very block it
reports), so `notify_queue` parks it and audits `notice-suppressed` with `parked: true`, relaying it
on that pane's next MCP tool result. Three reasons it does not belong in `refused`, and they are
worth stating because "a queue notice that was not delivered" *sounds* exactly like this list's
subject:

- **It is a different event.** Suppressed because of who the target is, not refused at the cap. The
  filter excludes it structurally — different action (`notice-suppressed`, not `delivery-dropped`)
  and different reason (`target-is-orchestrator`, not `queue-full-at-call`) — so this is a property
  of the derivation, not a special case bolted onto it.
- **It is not lost.** It is parked and relayed, which is the whole point of #624. Listing a delivered
  notice in a list documented as "nobody ever received this — re-send what still applies" would
  manufacture duplicate notices, the same failure the `recovered` exclusion above exists to prevent.
- **It is a notice ABOUT a delivery, not a delivery.** If the payload it describes was itself
  refused, that refusal already has its own row here; surfacing both would report one event twice.

The residual is #624's, not this list's, and it already owns it: the inbox is in memory, so a notice
parked and not yet relayed is lost across a restart — which is why its own audit line records the
notice text rather than merely that one existed, and why the relay block points a reader at
`audit.jsonl`. Its `ORCH_NOTICE_INBOX_MAX` overflow is likewise counted (`elided`) and named in the
relay wording rather than silent.

One interaction does run the other way and is worth recording: `notice-suppressed`/`notice-relayed`
are written per queue notice, so #624 raises a busy group's audit volume — which makes reaching
`AUDIT_VIEW_LIMIT` *more* likely and `refused_window_truncated` (NB1, above) more load-bearing than
it was when it was written, not less.

**Overlap with #569's resume notice, on purpose.** A refusal inside a pause window is also reported
by `announce_pause_suppression` at resume (`SuppressedCause::QueueFullDuringPause`). That notice is a
single best-effort delivery into the very pane that was full; this list is a durable, re-readable
derivation. The row's `enqueue_reason` (`group-paused`) is what tells a reader the two are describing
one event, and the duplication is worth it in the direction it runs: a notice that never landed is
exactly the case #579 exists for.

### Every refusal, not just the capped one (#633)

**The list above could only surface one reason, because one reason was all that wrote a line.**
`front_door_refusals` scanned for `delivery-dropped` with `reason: "queue-full-at-call"`, which was
not a narrow filter — it was the complete set of refusals that left any record at all.
`deliver_prompt_as` refuses two other ways, both BEFORE admission, and both returned `Err` in
silence: the target agent's status is `Dead`, and the target agent has no `pty_id` bound yet. The
second was *created* by #569/#615, which replaced a silent `Ok` — a payload discarded while its
sender was told it succeeded — with a silent `Err`. That was a strict improvement for the sender and
no improvement at all for anyone reading the log afterwards, because a refusal with no audit line
cannot be surfaced by this derivation, by the id-keyed orphan derivations, or by a human grepping
`audit.jsonl`. It is the #579 class exactly: *a refusal that leaves no record can never be surfaced
by anything.*

**Reasons are typed, because each one is a different instruction.** `RefusalReason` (`mod.rs`) is
the discriminator: `queue-full-at-call` (the pane is alive and busy — worth re-sending once it
drains), `agent-dead-at-call` (that pane will never take it — re-target or drop as stale, do NOT
re-send as-is), `no-terminal-at-call` (it was simply too early — re-send now that the pane has
bound), and `no-app-handle`/`registry-not-shared` (loomux itself could not process the queue —
report it, it should not happen in a running build). Collapsing them into one "refused" bucket
would have made the list enumerable and still not actionable, which is the shape #579 rejected for
orphans and would have re-introduced here. The filter is now a match over that enum via
`RefusalReason::from_audit`, so what it *excludes* is enumerated rather than implied: `agent-died`
and `queue-full` (whole-queue drops) carry an `id` and are already reported by the orphan
derivations, and an unmodelled reason from a future build is skipped by the same rule rather than
folded into a list whose documented response is "re-send".

**One refusal is deliberately NOT in that set, and the set is not a complete enumeration of
`deliver_prompt`'s failures without saying so.** `unknown agent` — the caller named an id that does
not exist — stays unaudited *structurally*, not by oversight: `audit` writes into a **group's** log,
`deliver_prompt` is keyed by agent id alone, and an agent that does not exist has no group to file
the line under. Writing it to a "no group" bucket or to the caller's own group would put a record
where nothing reads and no derivation scopes. It is also the one refusal that loses nothing an
operator could act on — no pane, no session, no payload owner to re-target to, only a caller that
named a nonexistent id, and that caller is told synchronously. Recorded here because a reason set
presented without its exception reads as the complete list of ways a delivery can be refused, which
is the implied-complete enumeration this whole section exists to stop (#633 review NB1).

**`queue_depth` and `enqueue_reason` became nullable, and that is the honest shape.** Neither
pre-admission refusal reached the queue, so there is no depth to report. Reporting `0` would be a
measurement nobody took, presented as one saying the pane was empty — the "a claim is a
deliverable" defect in wire form, and the same argument that rejected a synthetic orphan id in
#579. `null` there means "not measured", and `queue_orphans`' description says so in those words.

**Where the payload comes from differs by reason, and it has to.** #579 recovers a refused payload
by pairing the refusal with the delivery's own `prompt` audit line, verified on two fingerprints.
That works for `queue-full-at-call` and for the withdrawal reasons because `deliver_prompt` has
already written that line by the time either fires. The two pre-admission refusals happen *before*
it, so they carry their own `text` inline and the derivation reads it verbatim — same line, same
write, no join to get wrong and so nothing to verify against. **Moving the `prompt` write above
those refusals was considered and rejected**: `prompt` is what the whole backend suite (and
`delivered_texts`) reads as "this payload was offered to a pane", and a delivery to a dead agent
was never offered to anything. Widening it would have made every such refusal look like an
attempted delivery in the audit viewer and in dozens of existing assertions — a much larger and
much less honest change than one extra field on a line that is written only when something is
actually lost.

**The no-app-handle drop is SURFACED, not excluded — reversing #630's provisional call.**
`withdraw_unprocessable` undoes an admission that has nothing to drain it, and #630 skipped it here
on the argument that it is unreachable in production: `set_app` and `set_self_arc` both run in
`lib.rs`'s `setup` block, before the `mcp::serve` thread that is the only way an agent can call
`deliver_prompt` at all. That argument is *true today* and it is still the wrong basis for silence.
Two things decided it:

- **Once the list is reason-discriminated, the exclusion buys nothing.** It is one arm of a match
  that already exists. #630 was weighing it against a list that could only be about queue depth.
- **"Unreachable in production" is a claim about startup order that nothing enforces**, and the
  failure mode of it going stale is precisely the class this lineage is closing — a loss nothing
  can enumerate. Surfacing it means a broken assumption arrives as a row, not as silence. The
  tool description tells the reader to treat one as a loomux defect rather than as a payload to
  re-send, which is the honest instruction for a row that should not exist.

It cannot double-report: the withdrawal's line carries the `id`, which CLOSES that id for
`queue::orphaned_queue_entries`, so the entry has already left the orphan derivation by the time it
appears here. One loss, one row — the same rule the `recovered` exclusion enforces from the other
side.

**Two smaller corrections in the same seam.** Both `withdraw_unprocessable` call sites used to
withdraw under the reason `no-app-handle`, so the registry-not-shared arm wrote a line naming a
cause that had not fired (an app handle plainly existed — that arm sits *below* the one that
unwrapped it). Each now carries its own reason. And the withdrawal's pop is conditional — the id
may no longer be at the front — where the `else` branch wrote nothing at all, leaving a sender told
"failed" while its payload sat queued and no line joining the two. It now writes
`delivery-withdraw-missed`, and the **action is the load-bearing part**: `orphaned_queue_entries`
closes an id on `delivery-dropped`, so reusing that action would have told the orphan derivation
that a still-queued entry was resolved — a silent gap upgraded into a false all-clear. The entry
stays an orphan (correctly — it is still there) and `front_door_refusals` skips it for the same
one-loss-one-row reason.

**Boundary with #636, which is sharper than "they don't overlap".** #636 (#590 layer 2) is about a
delivery that IS queued and cannot be *typed* — a pane holding a `[loomux]` notice while mid-turn.
Its subject always has a live queue id and belongs to the hold-episode and orphan machinery; a
payload can be refused (no id, this list) or held (id, that one), never both, so **the held payload
cannot appear in both lists**.

What that formulation misses, and what landing the two together actually surfaced: #636's
escalation *notice* — the one that reports the held delivery — is itself a delivery, and it can be
refused like any other. Those are two different events about one situation, and both being visible
is correct rather than a double-report: the held payload is reported by #636 (still queued, will
land), and a refused notice ABOUT it is reported here (never queued, lost). `notice-undeliverable`
is a different audit action from `delivery-dropped`, so the derivation keeps them apart
structurally, which is what `an_undeliverable_notice_is_never_reported_as_a_front_door_refusal`
pins from #636's side. That test needed its fixture — not its assertion — corrected when #633
landed: it left the orchestrator with no pane, so the escalation notice to that pane was genuinely
refused, and pre-#633 that loss was simply invisible.

#638 (#632) is about the row markers on loomux-authored notice *text* and touches no
delivery-outcome record at all. Neither sibling overlaps the reason set above.

### Drain-time refusal roster (#658)

**Problem.** Everything above makes a refusal *enumerable*; nothing made it *arrive*. The sender
got a synchronous error, the loss got an audit line, and #578's rider told an orchestrator its own
queue was `FULL` — but no channel ever named WHO was refused to the pane that refused them. The
push paths and the pull path each covered the other's blind spot except in the middle: a
mid-session refusal to a live, later-draining pane. `queue_orphans`, whose contract frames it as a
start-of-session recovery step, was the only way to see one, and the live instance recovered a
worker's done-report only because the orchestrator happened to poll it.

**The trigger is an edge, and which edge is the whole bound.** `note_queue_capacity` already
detects capacity transitions for the badge and the pressure notices; the roster hangs off the one
transition where the depth came back DOWN (`CapacityState::Full` → anything else). That choice
does three jobs at once: it is the first moment there is room to tell the pane anything, it is
inherently once-per-drain rather than once-per-refusal, and — because a delivery can only push
depth *up* — it is the one edge the roster's own delivery can never produce. The emission is the
last thing that transition does, so the nested `note_queue_capacity` the enqueue triggers sees the
state this call already committed rather than racing it.

**The audit log is the record; the watermark is the only new state.** Every refusal already writes
`from`, `to`, `reason` and a bounded `preview` (#563/#633), which is exactly a roster row — so
`refusal_roster` derives from the timeline and `refusal_row` is shared with `front_door_refusals`,
rather than a parallel "not yet relayed" list that a restart empties and that can disagree with the
log a human reads. The one fact the log did not hold is how far the last roster got, so that is
written back as a line of the same log (`refusal-roster`), carrying `through_ms` **and**
`at_through`. Both terms are load bearing: a bare timestamp compared with `>` drops a refusal
stamped in the same millisecond as the last one reported — a report burst into one pane is
precisely when that collision happens — and `>=` repeats one forever. Recording how many of the
covered refusals shared the newest millisecond makes the skip exact, because audit entries are
appended in write order and a same-millisecond refusal appended *after* the roster ran is simply
the next one. Only a line that also says `delivered: true` advances the mark, so a roster that was
itself refused loses nothing.

**"Already re-sent" is derived, and marked rather than suppressed.** `refusal_was_resent` walks
forward from the refusal counting later `prompt` lines that match its two fingerprints
(`text.len() == bytes`, recomputed `dropped_payload_preview`) and spending them on later refusals
of the same payload; a surviving credit means one attempt was admitted, or coalesced onto an entry
already queued — the same fact for the recipient. Only the queue-full *shape* spends a credit, because
the #633 pre-admission refusals write no `prompt` line at all. It inherits #579's pairing residual
(a different payload agreeing on length and on its collapsed first 160 chars) and answers `false`
whenever nothing can be established, since the cost of being wrong that way is one extra ask rather
than a pane told a lost report is handled. The rows stay in the list because the recipient cannot
tell a re-send from a first send, and a silently shortened list reads as "these are all still
missing" while being short.

**Two channels, one string.** An orchestrator's own pane is parked in `OrchNoticeInbox` and rides
its next tool result, for #578's reason; every other pane is told by an ordinary delivery under
`EnqueueReason::RefusalRoster`, whose `cap_headroom` is **zero**. That is the opposite call to
`PauseLossNotice` and the difference is the trigger, not the importance: the loss notice fires at a
resume, which says nothing about depth, so on the pane it is about it is *certain* to be refused
without an exemption — the roster fires where depth just fell, so it has room by construction. A
roster refused anyway is recorded `delivered: false` and re-derived at the next drain. Because both
channels carry one string, and `OrchNoticeInbox::park` requires a single marker-led line, the
roster is one line: `ROSTER_LIST_MAX` (4) rows at `ROSTER_PREVIEW_MAX` (80) chars, with the
remainder counted out loud and left in `audit.jsonl`.

**Not `mark_notice_maskable` (#661), deliberately.** That call is a per-field producer promise that
every span was composed by loomux or called in by an agent other than the recipient. A roster's
previews are payload text authored by whoever sent the refused delivery, echoed into the
recipient's own pane, so the promise is false on its face — and claiming it would let a sender
place chosen text into another pane's masked record, which is the injection surface the
default-closed rule exists to keep shut. It keeps the default, like the queue notices beside it;
the cost is a gate that holds slightly too long, never one that releases early. The single-line
rule `park` asserts is a different mechanism (#576's pane-tail masking of loomux's own framing) and
is satisfied structurally, by leading with `[loomux]` and never wrapping.

**Anti-recursion, stated twice because a refusal has two shapes.** A refused roster is excluded
from the next roster by its `enqueue_reason` when it reached admission, and by
`REFUSAL_ROSTER_OPENER` leading the preview loomux itself wrote when it did not (the pre-admission
refusals record no reason at all). It is still listed by `queue_orphans`: a refused roster is a
real loss, and hiding it from the pull path to keep the push path tidy would be a second silence.
Marker refusals (`RefusedPayload::StrandedSubmit`) are excluded for the opposite reason — no sender
to ask and no payload to re-send, and their `consequence` already says the actionable thing.

### Coalesced flush: one drain pass, one prompt (#533-A)

**Problem.** The queue above solved "hold means queued, never doomed" and then charged for it by
the turn. A drain pass planned exactly the FRONT entry, so a pane blocked behind an unanswered
question for an hour woke up to N separate prompts — each a full paste/echo/confirm cycle and,
more expensively, a full agent turn to read. The queue was doing its job; the flush was billing
for it N times.

**Approach.** The plan is made over the WHOLE queue, once per pass. `queue::plan_flush` (pure)
takes the pane's queue oldest-first and returns what this pass submits — a batch of ids, the ids
that drop out first, and how many entries remain for the next flush.
`queue::coalesced_flush_text` renders the batch as ONE prompt: the flush header, then each
constituent behind a banner naming its position, origin (`from`), queue age, queue id, and any
byte-identical repeats `admit` folded in at admission. `run_queue_drainer` pastes that once.

Four constraints shape it, and each is a rule in the planner rather than a convention:

- **Order is the queue's order.** Nothing is rearranged, so the header can say so. This is the
  same property "Ordering" (above) proves for admission; coalescing preserves it trivially by
  never reordering a batch it took in order.
- **Nothing is ever held back to be batched.** The batch is only ever what is ALREADY queued at
  that instant. There is no batching timer and no path that waits for a second entry — a lone
  live delivery plans a one-entry batch and pastes exactly as it did pre-#533, wording included.
  A delivery that lands while a flush is in flight simply flushes on the next pass.
- **Size-capped, and the cap is a ceiling not a floor.** `QUEUE_FLUSH_MAX_BYTES` (24 KiB) splits
  a large backlog into consecutive flushes, and the header states how many follow — a paste is
  echoed and re-read, so an unbounded combined payload would trade the turn cost this saves for
  a worse context cost. A single constituent larger than the whole budget still delivers, alone:
  a cap that can stall a queue is a destruction path with extra steps. **This cap now has a second
  consumer (#559)**: Tier 1's box-scan ceiling is derived from it, so the window loomux reads to
  verify its own paste can always contain what one flush may produce. Changing this number moves
  that read too — deliberately, since the two being independent is precisely what let an oversized
  flush go unverifiable and silent. See "The scan window was the other half of the precondition".
- **#451's three-state confirmation transfers to every constituent.** One paste is one submit is
  one `submit_sent_ms`, which is one `last_delivery` outcome — so it is the outcome of every
  constituent it carried. `pop_batch_dequeued` closes out the whole batch on `Done` and audits
  each id with `combined_ids`, so a reader of `audit.jsonl` can tell one paste from three. The
  pre-Enter abort path is the same argument in reverse: the box holds the whole batch, so the
  whole batch closes out and ONE stranded marker stands for all of it. Popping only the front
  there would re-paste text already on screen the moment the marker's Enter lands.

**What "supersession applies per-constituent, before coalescing" means here — stated narrowly on
purpose.** #454's supersession is per-PANE ledger state (`last_delivery`'s `submit_sent_ms`),
which an entry that has never been pasted cannot carry; there is no per-entry supersession field
to read, and inventing one would have produced a rule that can never fire — which is worse than
no rule, because it reads as coverage. What genuinely drops out before anything merges is exactly
three things:

1. A `StrandedSubmit` marker never merges and terminates the batch (`plan_flush`) — its text is
   already in the box, so it submits alone and whatever is behind it flushes next cycle.
2. A dead target or closed pty drops the whole queue before a plan is computed — the pre-existing
   `commit_exit(force: true)` path, unchanged.
3. A drain-time byte-identical re-check (`superseded_entries`): the rule `admit` applies at
   admission, applied once more over the batch. Coalescing changes what a duplicate that raced a
   pop costs — as its own prompt it was visibly redundant; merged into one paste it reads as two
   distinct asks. The LATER duplicate drops and the earlier keeps its place.

   Two details rev-13 was right to press on. **The drop MOVES the duplicate's coalesce count onto
   the survivor** (`Superseded { id, by }` pairs each drop with the entry that absorbs it): a
   drain-time drop that only removed the entry would under-report "+N identical repeats" for
   exactly the case this re-check exists to catch, in a feature whose point is attribution
   completeness. The transfer happens under the same lock as the removal, and the drainer re-reads
   the queue afterwards so every number it renders — per-constituent counts, the single-entry
   header's own depth — is post-drop rather than from a snapshot that predates it. **And the drop
   is audited without a `[loomux]` notice**, unlike `announce_dropped` and the queue-full case in
   `AbortedPreEnter`, which send both. Those two lose a payload; this one does not — the identical
   text still delivers via the surviving entry, in its original position. There is no loss to
   announce, and announcing one anyway would spend the exact orchestrator turn this PR exists to
   save. `superseded_by` and `folded_coalesced` on the audit line keep the transfer
   reconstructible.

**Residual, stated:** `run_queue_drainer` itself is still not drivable in-suite (no live
pty/`AppHandle`, and constraint 3 forbids a real CLI), the same boundary every delivery test in
this design has. The planner and every notice string are pure and directly tested; the registry
seams the drainer calls (`pop_batch_dequeued`, `drop_superseded`) are driven against a real
registry; the literal call between them is covered by a `debug_assert` tying `plan_flush`'s
`stranded` flag to the planned front entry's own payload, not by a test.

### #563: the hold nobody could see, and the queue that filled in silence

**The report.** A copilot orchestrator's deliveries were held against a pane whose prompt box was
**empty**, with **no warning** anywhere in the UI, until the delivery queue **filled completely**
and work was dropped. Three fixes already existed for pieces of this — #246's held chip, #532's
bounded question hold, #445/#523's queue persistence and orphan surfacing — and the combination
still happened.

**Cause 1: the chip covered the wrong holds.** `orch-delivery-held` was emitted from exactly one
place, `deliver_now`'s `emit_held` closure, around that attempt's individually-capped waits — and
cleared when the attempt ended, *including when it aborted*. But an attempt is bounded by
construction; the hold a human sits in front of is not. It lives in `run_queue_drainer`'s poll
loop, which re-reads `write_admission` every `QUEUE_DRAIN_POLL` and, on a blocked admission,
`continue`d with no UI event at all. So the chip's entire lifetime was the inside of one attempt,
and the sustained hold — minutes, hours — showed nothing until #532's escalation badged at
`QUESTION_HOLD_STALE_AFTER` (ten minutes, and by that constant's own doc up to ~19 in practice
because the bound is only sampled *between* attempts). On the affected build #532 had not shipped
at all, so there was nothing.

The fix is `HeldEscalation::Chip`: the same decision function the drainer already consulted now
returns a chip for every blocked poll inside the bound, instead of `None`. Two existing test
assertions had to change, and it is worth being exact about that rather than quietly editing
expectations — **as written, they pinned the invisible window.** One asserted that a hold inside
the bound reports nothing; the other that a writable pane has nothing to clear (true only while a
chip could not exist without a badge). Neither guarantee is lost: the escalation still fires only
at the bound, and "no spurious audit on a healthy poll" moved to the caller's guarded clear, which
is where it belongs now that `Clear` fires on every writable poll. A `ChipGuard` (RAII) drops the
chip on every exit from the drainer, including the early returns for a closed pty or a dead
agent — a stale "held" chip on a pane nothing is holding would be worse than none, because it
trains a human to ignore the real one.

**Cause 2: on an orchestrator pane, the queue's whole notice channel is a no-op.** `notify_queue`
returns early with `notice-suppressed` whenever the target IS the group's orchestrator. That
suppression is *correct* and structural — a notice to the orchestrator about the orchestrator's
own blocked pane would queue behind the very block it reports — but it covers `queued_notice`,
`still_queued_notice`, `dropped_notice` and `recovered_notice` alike. So a queue could fill to
`QUEUE_MAX_PER_PANE` and start rejecting payloads with the human told nothing: the rejection's only
other signal was a synchronous `Err` to the *calling agent*. `StrandedBlocker::QuestionStale`'s own
doc predicted this ("**nobody is told until someone next looks at the window**"); #563 is that
prediction one channel over.

The rule this settles, and the one `hold_channels` now enforces: **a hold classification whose only
channel is the in-band notice is invisible exactly where it matters most.** Every classification
must have at least one channel that survives an orchestrator target — a chip or a badge, both of
which are UI events keyed on `pty_id` with no role suppression anywhere in their path.

**The enumeration is executable, not prose.** `HoldClass` (13 variants), `HoldChannel`, and
`hold_channels` — matched exhaustively, so a hold added later cannot be silent by omission, and
tested by `every_hold_class_reaches_a_human_who_is_not_the_orchestrator_channel`. A table in this
file would have rotted the same way the old implicit one did. `HoldClass::ordinal`'s exhaustive
match plus a hardcoded `ALL.len()` assertion is what keeps `ALL` honest: a new variant fails to
compile until it has an index, and fails the test until it is listed.

**Reconciling the investigation's 17 rows to the 13 variants.** The #563 investigation published a
17-row table of hold *sites*; `HoldClass` has 13 *classifications*. The mapping is precisely where a
row could silently go missing, so it is written down here rather than left to be re-derived — the
"enumerate in writing, the already-fine ones included" rule from `.loomux/lessons.md`, applied to
the enumeration itself.

| Investigation rows | `HoldClass` | Why |
| --- | --- | --- |
| 1, 2, 3, 7, 8 | `PrePasteTyping`, `PrePasteBoxOccupied`, `PrePasteQuestion`, `PreEnterTyping`, `PreEnterQuestion` | direct 1:1 |
| 6, 9 | `PrePasteRecheckExhausted`, `PreEnterBoxOccupied` | direct 1:1 |
| 10, 11 | `QueuePollBoxOccupied`, `QueuePollQuestion` | direct 1:1 — the #563 defect |
| 12 | `QueueStaleEscalation` | direct 1:1 |
| 17 | `GroupPaused` | direct 1:1 |
| **4, 5** | *no variant* — subsumed by rows 10/11 | These are the pre-paste **aborts**, i.e. the terminal transition of rows 2/3. After an abort the entry stays at the front of its queue and the drainer's poll is what looks at it next, so whatever still holds it is reported as `QueuePoll*` — which now chips from ~2s. An abort is a handoff, not a resting state, and a resting state is what a classification names. |
| **13** | *no variant* — backstopped by 10/11/12 | The 30-minute still-queued notice rides the same suppressed in-band channel, but a pane still queued at 30 minutes is still queued **because** the drainer is blocked — so rows 10/11's chip and row 12's badge have both already fired long before. It adds detail to a report that exists rather than being the only report. |
| **14, 15, 16** | all → `QueueFull` | Three sites (front door, marker push, recovery re-admission), one classification: the queue is at cap and an arrival was refused. All three reach `note_queue_capacity`, so all three badge; 16 additionally stages into `queue_orphans` (#523). |

Rows 4/5 and 13 are the only two places the counts differ, and both are subsumptions rather than
gaps — which is the fact worth being checkable at a glance instead of reconstructed.

**Capacity: a state between "fine" and "work has just been lost".** There was none, which is why
nothing could warn while warning was still useful. `queue::CapacityState` adds `Approaching` at
`QUEUE_NEAR_FULL_AT` (6 of 8). Two slots of headroom, sized against what the warning is *for*
rather than against traffic: at a 2s drain poll a held pane admits arrivals as fast as its senders
produce them, so a one-slot threshold would routinely be crossed and exhausted inside a single tick
and the "warning" would arrive with the loss.

`note_queue_capacity` is the one place that reports it, called from every path that changes a
pane's depth (admission, rejection, marker push, dequeue, batch dequeue, supersession, whole-queue
drop). It is **edge-triggered** — a pane parked at 7/8 for an hour produces one audit line and one
badge, not one per arrival — and it **releases on evidence**, a depth that has actually come back
down, never on elapsed time (`.loomux/lessons.md`: a queue still full after ten minutes is more
urgent, not less). The remembered record carries the agent id alongside the state, and that is
load-bearing rather than convenience: the release is observed on a pop that may have just emptied
the queue, so at the moment the badge must come down there is no queue entry left to say whose it
is.

**The two capacity badges say only what depth establishes.** `note_queue_capacity` decides on the
queue's length and nothing else — it never reads `write_admission` or any hold state — so its badges
are `StrandedBlocker::QueueNearFull` and `QueueAtCapacity`, both minted for this path, and **not**
`QueueFull`. `QueueFull` is `actuate_stranded`'s: its wording says loomux *could not even queue a
re-send* and tells the human to *press Enter in the pane*, which are true there (a self-heal really
was refused; stranded text really is in the box) and unbacked here.

The first cut got this wrong in a way worth recording, because it is the exact failure class this
issue exists to eliminate, arrived at from the other side. Both capacity badges read "the pane **is
held** — press Enter or answer what's on screen". But a pane whose drainer is perfectly healthy and
whose senders simply outrun it — a fleet of workers reporting into one orchestrator pane — reaches
these thresholds with **no hold of any kind**. The human is then sent hunting for a dialog that does
not exist, and a badge that wastes someone's time once is a badge they discount next time: the same
"trains a human to ignore the real one" harm the drainer's `ChipGuard` exists to prevent. The
wording now asserts the backlog (which depth does establish) and offers the hold as a **condition to
check** ("if the pane is held, releasing it drains the backlog").

Both badges are released on the same evidence — a depth that has actually come back down — and the
release is deliberately scoped to those two variants: `QueueFull` raised by `actuate_stranded` is
about a refused re-send, which a falling depth does not resolve, so clearing it here would drop
another mechanism's still-true badge. Equally, neither badge stomps a badge that is already up
(another mechanism is already telling the human to look at this pane); they only upgrade their own,
so `Approaching` → `Full` re-words rather than sticking at the softer sentence.

**A forced drop now names what dropped.** The pre-#563 line recorded `{to, reason, depth}` and no
more. No id is minted for a rejected entry, so there was nothing else in the log to join against,
and `audit.jsonl` rotates — that line is the only record that will ever exist. It now carries
`from`, `bytes` and a bounded, single-line, explicitly-truncated `preview`, because a sender can
re-send a delivery it can identify and cannot re-send an anonymous one. The marker-push rejection
names its own consequence in words (`text is pasted in the pane with nothing queued to submit it`),
which is the fact a reader actually needs from that line.

**Persistence (the #468 table above).** `enqueue_text` has three arms and **two** of them mutate
`queues`: `Admit` (the push) and `Coalesce` (the surviving entry's `coalesced` counter — under-report
that and a recovery loses the flush header's de-duplication count). Both already called
`persist_queues` and still do — since #562 by returning `QueueDirty::snapshot()` from the
`QueueMap::mutate` closure, which is the same write reached a way that cannot be skipped.
`RejectFull` mutates nothing the FILE carries — it reads `q.len()` and returns
`QueueDirty::nothing_persisted()` — so it owes no write. One nuance, stated because this arm is
where a reader would go looking for a lie: all three arms run `queues.entry(pty_id).or_default()`
first, which on a pane with no queue yet DOES insert an empty deque, so `RejectFull` is "mutated
the map, persisted nothing". That is sound and checkable rather than a judgement call —
`group_queue_entries` builds the snapshot by iterating each pane's *deliveries*, so an empty deque
contributes no entry and cannot change a byte of `queue.json`. Spelled out arm by arm rather than
left to inference, because "anything mutating `queues` owes `persist_queues`" is the rule this
subsystem is reviewed against and this paragraph is what a later reader audits against; an earlier draft of it said only `Admit` mutates,
which is exactly the claim that would later license skipping a persist on a coalesce-shaped path. `queue_pressure` is in-memory one-shot state like `question_stale_notified`
and owes no snapshot either; it is cleared in `commit_exit` under the same generation guard, or a
successor drainer's first reading would look like a fall from `Full` and emit a release for a pane
that never recovered.

**What this does NOT fix, stated rather than implied.** The interactive-question gate still reads
an append-only byte ring, so an answered question keeps matching until fresh output pushes it out —
and the drainer's own gate calls `question_active_now(.., None)`, i.e. without `mask_own_paste`,
because at that point the entry has pasted nothing of its own even though the *previous* delivery's
text is still in the ring. Text loomux itself delivered can therefore latch that gate. #563's fix
makes such a latch **visible within two seconds** instead of invisible for ten minutes; it does not
make the reading correct. The correct reading needs rendered rows (`termgrid`, #530) and is filed
separately. The other thing filed separately — a channel for orchestrator panes to carry what
`notify_queue` structurally cannot deliver — is #578, below.

**Residual.** `run_queue_drainer` is still not drivable in-suite (no live pty/`AppHandle`), the same
boundary every delivery test in this design has. The decision it consults (`held_escalation`) and
every registry seam it calls (`note_queue_capacity`, `pop_front_dequeued`, `mark_stranded`) are
driven directly; the emit calls between them are not.

## The orchestrator's notice inbox: a channel that is not a delivery (#578)

**Problem.** `notify_queue` returns early with `notice-suppressed` whenever the target IS the
group's own orchestrator, and #563's section above establishes why that early return is *correct*
and must stay: a prompt announcing the orchestrator's own blocked delivery would queue behind the
very block it is reporting. What was wrong is what happened next — **the notice was discarded.**
#563/#572 answered that with channels which survive an orchestrator target (the held chip, the
attention badge), and both of them reach a **human at the window**. So an orchestrator pane's queue
events reached the human-if-someone-is-looking, and nobody else; on an unattended overnight run
they reached nobody at all, which is the consequence `StrandedBlocker::QuestionStale`'s doc had
already written down in those words.

**The channel.** A queue notice suppressed for an orchestrator target is **parked** in
`OrchRegistry::orch_notice_inbox` (keyed by group — its reader is whoever `deliver_to_orchestrator`
would have delivered it to, which is a role and not a fixed id) and handed back as an extra MCP
**content block** on that orchestrator's next `tools/call` result. Three properties, and each one
is the answer to one of the issue's three questions:

- **It costs no pane delivery slot, so it cannot reintroduce the loop.** Nothing is typed into the
  blocked pane and nothing is enqueued for it. The relay rides back on a call the orchestrator
  *itself* made, which is also the proof that the orchestrator is running and reading at that
  instant — the one moment an in-band notice is guaranteed not to be queued behind anything.
- **Yes, the orchestrator learns about its OWN pane** — that is the whole case, and the wording
  says so (`YOUR OWN pane`), because every other `[loomux]` notice an orchestrator reads is about
  somebody else. It also says plainly that nothing needs re-sending: the payloads these notices
  describe are either already queued and delivering or already gone, so a re-send is a duplicate in
  the first case and a guess in the second.
- **It is a pull, not a push, and is listed ALONGSIDE the badge, never instead of it.** An
  orchestrator that never calls another tool never reads its inbox. The badge and chip reach the
  human regardless; the inbox reaches the agent on its next turn. The two cover different failures
  (a human who is not at the window vs. an agent that is mid-turn and busy) and neither subsumes
  the other, which is why `hold_channels` lists both.

**Attached at the `tools/call` funnel, and as a SECOND block.** The relay is attached in `dispatch`,
at the one point every tool call passes through — the same reason `note_agent_ack` is stamped there
(#535): no per-tool opt-in to forget. It is a second content block and never an append to the
first, because several tools return JSON their caller parses (`queue_orphans`, `get_state`,
`group_usage`) and a notice glued onto that string would corrupt it. It is attached on an `isError`
result too: the orchestrator is demonstrably alive and reading either way, and withholding the
notice because an unrelated call failed would put it straight back in the hole this fills.

**Drained once, under the lock.** The entry is `remove`d, so two concurrent `tools/call`s cannot
both relay the same notice and a relayed notice cannot repeat on the next call — a relay that
re-fired every call would be a nag, and an orchestrator's context is what pays for it.

**In memory, because the durable record is the audit line.** The inbox is a relay for the *next
turn*, not a store. The `notice-suppressed` line now carries the notice `text` (capped) and
`parked: true`, so a loomux restart loses the relay and not the information — the same split #467
uses, where `queue_orphans` is the durable channel and the `[loomux]` notice is the best-effort
one. The cap (`ORCH_NOTICE_INBOX_MAX`, 20) evicts **oldest first** — the newest notice is the one
whose claim is still true — and **counts** what it evicted, which the relay then states along with
a pointer at the log that still holds every one verbatim. A relay that silently held back N notices
would read as complete while being short, which is the defect class this whole lineage exists to
eliminate.

**Branch order: an orchestrator target during a pause IS parked.** `notify_queue` checks
`target_is_orchestrator` *before* `is_paused`, so the paragraph below is about a **worker** target
during a pause; an orchestrator-target notice never reaches the pause branch at all. That order is
deliberate rather than incidental (review NB2), and `note_queue_capacity` on the orchestrator's own
pane is the live path that exercises it. A pause suppresses *deliveries*, and the relay is not one:
a paused orchestrator can still call tools, and its own pane's queue pressure is exactly what it
should learn about while everything else is held. Pinned by
`during_a_pause_a_worker_target_is_only_audited_but_an_orchestrator_target_still_parks`, which
asserts both branches — the earlier test asserted only the worker case while its name claimed the
general one.

**What is deliberately NOT parked.** `notify_queue`'s *paused-group* branch — and #615
(enqueue-while-paused) sharpened that decision rather than weakening it. Since option 2 a pause is
a **delay, not a discard**: the deliveries behind it sit in the pane's durable queue and are
flushed in arrival order at resume, with `flush_header_text` announcing them at that moment. A
queue notice suppressed while paused therefore describes an event whose payload is safe and which
the resume flush reports *in time and accurately*; relaying it minutes later out of a parked buffer
would say "queued" about a delivery that has since landed. The one loss a pause on this build can
still cause — a pane already at `QUEUE_MAX_PER_PANE` refusing admissions for as long as the pause
lasts — has its own channel in `announce_pause_suppression` and its badge fallback, so parking here
would be a second report of a fact that already has one.

For the same reason `hold_channels` gives `HoldClass::GroupPaused` no `OrchestratorInbox`: its
notice is a different call on a different path that `notify_queue` never sees, and listing an inbox
it never parks into would make that table say something untrue.

**Reconciled with #615 at the cap, which is the sharpest way to see what this channel is.** #615's
pause-loss notice *is* a delivery, so on a pane at `QUEUE_MAX_PER_PANE` — exactly the pane it has
the most to report about — it was **certain** to be destroyed by the same cap it was reporting, and
needed `EnqueueReason::PauseLossNotice`'s one entry of headroom (`PAUSE_LOSS_NOTICE_HEADROOM`) to
survive. The inbox never meets that problem and takes no headroom of its own: it enqueues nothing,
so a full queue is the *condition it reports*, not an obstacle to reporting it. The two mechanisms
are complements, not alternatives — #615 buys a delivery its way past the cap; #578 declines to be
a delivery at all — and neither is a partial version of the other.

**Reconciled with #621 at the notice marker, which is the seam nobody would look for.** #576's
self-latch is loomux's own notice text sitting in a pane's tail and satisfying the question
detector; #621 fixed it by masking any row that *leads* with `LOOMUX_NOTICE_MARKER` once
`deframe`d, at every reader of a live pane. The relay block is not one of those readers — it rides
an MCP tool result and never touches the pty — so nothing here needed a mask. The interaction runs
the other way: #621's own argument records that **an agent can print marker text itself**, and an
orchestrator quoting its relayed queue notices back into a summary is exactly that, in the pane
most exposed to the latch. So the relay is shaped to stay *maskable*: the header and every
constituent notice already lead with the marker, the bullet is `•` (in `deframe`'s strip set) and
not `-` (which is not), and the elision line carries the marker instead of opening with prose.
The invariant is enforced from **both ends**, because they fail differently (review NB3).
`every_row_of_the_relay_block_is_maskable_by_the_question_gates_notice_rule` pins how the block is
*rendered*, so a later edit that reintroduces a `-` fails rather than quietly handing one row back
to the detector. `every_notice_that_can_reach_the_inbox_is_a_single_marker_led_line` pins what may
go *into* it — the six notice constructors across `notify_queue`'s seven call sites — because a
constructor growing a second line would break maskability with the renderer and every call site
unchanged. That list is maintained by hand, which is exactly why `OrchNoticeInbox::park` also
carries a `debug_assert` written *through* `mask_loomux_notices` itself: a seventh constructor
added without touching the list still fails, at the door, in any debug build (and CI's test builds
are debug). Release builds never pay for it and never panic a live session over it — the degraded
outcome is a gate that holds too long, which `QuestionStale` already reports.

**Residual, and its boundary was filed rather than implied — then closed.** `mask_loomux_notices`
claims one row per marker, so any loomux text that occupies *several* rows keeps everything past
the first — `pause_suppression_notice` and `coalesced_flush_text` are the real multi-row cases, and
their continuation rows rode the pty unmasked. That was **#632**, filed out of this review rather
than half-addressed here, and it is named in this section because the boundary is what made this
change reviewable: #578 is single-row by construction and adds no pane rows at all (the relay rides
a tool result, never the pty), so it neither contributed to #632 nor was blocked by it. #632 has
since applied this section's own convention to both producers — see "*the multi-row notices, fixed
at the producer rather than at the mask*" above. It did **not** widen the mask, which is why that
fix and this one compose: the sentence "closing this needs loomux to know *what* it wrote to a
pane" survived #632 intact and described only the wrap and scrolled-off residuals — which is
exactly what the per-pane delivery record then built (see "*masking on what loomux WROTE*" above).
That change did not widen the marker rule either: it left this section's one-row-per-marker
convention untouched and added a second, record-anchored rule beside it.

`HoldChannel::survives_orchestrator_target` also stopped being a `!matches!` negation and became an
exhaustive match: a channel added later must state its own answer rather than defaulting to
"survives", which is the optimistic half and the one that would make
`every_hold_class_reaches_a_human_who_is_not_the_orchestrator_channel` pass by accident.

**Residual.** An orchestrator that is wedged on a question and never calls another tool never
drains its inbox — by construction, since this channel is a pull. That case is the badge's and the
chip's, and it is why they remain listed. The desktop-toast path (`notify_desktop`, opt-in per
group via the attention scan) is unchanged and still rides the badge.

## A notice the pane cannot take is a fact loomux can see (#590 layer 2)

**Problem, from a live incident (PR #577, this group).** A worker registered a `notify_when` CI
watch — correct — and *also* blocked its turn on a shell-level wait for checks on its new head.
Two merges had landed underneath it, so the PR was `CONFLICTING`, and GitHub never creates
check-suites for a PR with no clean merge ref: the shell wait was on a condition that could not
occur. Meanwhile the watch fired and `notify::watch_conflicting_notice` — which #337 built for
exactly this case — was queued for that pane. A `[loomux]` notice is delivered by **typing into a
pane**, and a pane mid-turn cannot take a delivery. So the turn waited on a resolution that was
queued behind the turn itself, and the one channel that could have broken the deadlock was the one
the deadlock blocked. 20+ minutes, ended by the host watchdog plus a human reading the pane.

**Layer 1** (PR #594, `75e9a07`) is the delegate rule — register the watch, END the turn, act on
the notice — now in `worker.md`, `reviewer.md`, the `ci-validate` skill and `.loomux/lessons.md`.
It is a rule about what an agent should not do, which is the cheap half and not a mechanism.
**Layer 2, here,** is the observation an agent *cannot make about itself*: from the host side this
state is fully visible, and before this nothing looked.

**What the pre-existing escalation reached, and why neither arm covered it.** The hold was already
detected: `hold_escalation_step` opens a per-pane hold episode (#560) and escalates at
`QUESTION_HOLD_STALE_AFTER` (ten minutes). But `HoldClass::QueueStaleEscalation`'s channels are the
held chip and the attention badge, and both reach **a human at the window** — the exact reader an
unattended run does not have, and the reader whose absence is what made the incident cost 20
minutes. The one channel that reaches the orchestrator *agent* for a stuck pane is
`queue::still_queued_notice`, at **thirty** minutes: on this timeline it never fired, and had it
fired it would have said *"nothing lost, delivers automatically once clear"* — true of a queue in
general, and precisely the wrong thing to read about a pane that cannot clear itself.

**The gate: the held payload is loomux's OWN notice.** `queue::is_loomux_notice` requires **both**
`from == "loomux"` and a `[loomux]`-led payload, and each half covers the other's blind spot. `from`
alone over-matches — a kickoff brief is also from loomux and is *work*, with its own recovery
(#517/#585) — and a stale hold on work is an ordinary busy pane, already correctly served by the
chip and the badge. The marker alone under-guards, because agent text is relayed verbatim and
`LOOMUX_NOTICE_MARKER`'s own doc states the limit in these words: a marker row is evidence that
someone wrote a notice-shaped row, never proof that loomux wrote *this* one. Requiring both rests
the answer on a field an agent cannot set and a prefix loomux always writes.

That gate is what makes this a *different event* rather than a louder copy of the stale-hold badge,
and it is why it is a separate `HoldClass::UndeliverableNotice` rather than two more channels on
`QueueStaleEscalation`'s row. Folding them in would make the hold-channel table — whose entire
purpose is to be checkable — claim an orchestrator-facing channel for a held kickoff, which nothing
sends.

**The diagnosis, and the evidence behind it.** `undeliverable_cause` answers *who* is holding the
pane, because the drainer's two hold reasons (`box-occupied`, `question`) do not distinguish the
cases that need opposite responses: a human's half-written line (wait — a person is right there)
from a CLI's own turn state (do not wait — the pane will not clear until that agent's turn ends).
The precedence is **dialog → keystroke evidence → its absence**:

- `QuestionOnScreen` first, because a rendered dialog is a direct observation rather than an
  inference from absence — *and* because it covers the keystroke signal's documented blind spot.
  `PtyManager::last_user_input_ms` is stamped only for keystroke-like input (#496), whose stated
  tradeoff is that arrow keys and menu navigation classify `Neutral` with a zero occupancy delta and
  never stamp it. A keystroke-first order would report a human mid-menu as a pane mid-turn.
- `HumanTyping` when a keystroke landed **at or after the hold episode opened**. The comparison is
  against the episode rather than a fresh window, which is why this needed no new constant: the
  question worth asking is not "did a human type recently" (recently against what?) but "has a human
  touched this pane at any point in the whole time it has been refusing our delivery".
- `PaneMidTurn` otherwise — #590's shape. **What that claim is, exactly:** negative evidence ("not a
  human") plus the box reading. It is *not* proof that a turn is running, since `input_pending` is
  documented to latch over an already-empty box, and the notice's wording therefore states the
  evidence (`no human keystroke since the hold began`) alongside the diagnosis, so a reader can
  check it. Every response the wording invites — read the pane, do not wait on it — is correct under
  both readings.
- `Unknown` when the pty is gone or no gate is attributed. Named rather than folded into a
  neighbour, so a notice never asserts a cause it did not observe.

**One-shot per hold episode, and deliberately not the badge's one-shot.** `HoldEpisode` gains
`notice_reported` rather than reading `badged`, because the two come apart in *both* directions: a
badge RAISE sets `badged`, after which `held_escalation` returns `None` for the rest of the episode
(`if already_badged`), while a raise DECLINED — another mechanism already owns the badge — leaves
`badged` false and `Badge` comes back on every poll. Sharing the flag would therefore mean never
reporting after the instant the bound was crossed in the first case, and re-reporting every two
seconds in the second. The badge is the human's channel and this is the orchestrator agent's; they
fail independently and need independent state.

**Evaluated on the bound, and claimed by the poll that reports** (rev-128's blocking finding, and
the half its remedy did not reach). The report started life *inside* the `Badge` arm, claiming
`notice_reported` before it read the queue and returning at the zero-notice gate having already
spent it. Two defects compounded there. Claiming first meant an episode that crossed the bound
holding only *work* burned its only report; and living in the `Badge` arm meant that in the common
configuration — the raise succeeds, `badged` goes true — no later poll returns `Badge` at all, so
nothing looked again even after the flag was fixed. The failure is one step from #590's own
incident: an orchestrator follow-up held on a mid-turn worker opens the episode on work, the bound
elapses silently, the worker's own CI watch then fires and queues `watch_conflicting_notice` behind
it — #590's exact payload — and nothing surfaces it, for as long as the pane stays held.

So the call is keyed on `hold_bound_elapsed` and sits outside the `Badge` arm, and
`note_undeliverable_notice` reads the flag, then the queue, then claims. There was no tension to
trade off: `notice_reported` is set once and never cleared inside an episode, so re-asking on each
poll costs a re-read and never a second report. The steady state after a report is one flag read per
poll; before it, one flag read plus a snapshot of at most `queue::QUEUE_MAX_PER_PANE` entries, on a
thread already polling a pty every `queue::QUEUE_DRAIN_POLL`. Pinned by
`a_bound_crossed_with_nothing_queued_does_not_burn_the_report`, which runs in the badge-*raised*
configuration and asserts the escalation verdict is `None` at the instant the report fires — so a
fix that only reordered the flag inside the `Badge` arm fails it.

**Composition with #578, which is the case the issue actually turns on.** The notice goes out
through `notify_queue`, so when the stuck pane is the **orchestrator's own**, that function's
orchestrator-target branch parks it for the notice inbox instead of typing it into the pane it is
about — and it rides back on that orchestrator's next MCP tool result, a call the orchestrator
itself made, which is also proof it is running and reading at that instant. Nothing is enqueued for
the blocked pane on any path, so the notice cannot queue behind the block it reports; and a parked
notice cannot re-trigger this classification either, because parking creates no queue entry and the
episode's one-shot is already spent. Before #578 this exact case would have been written and
discarded.

**Boundaries with the neighbouring work, stated because each one is a live issue.**

- **#579/#630 (the `queue_orphans` refused list).** #630 landed while this was in review, so the
  boundary below is re-derived against `front_door_refusals` as shipped rather than against the
  issue's description of it.

  **Disjoint by construction, and the construction is now checkable.** That scan matches
  `action == "delivery-dropped"` **and** `reason == "queue-full-at-call"`, and nothing else — the
  filter says so in its own comment. `notice-undeliverable` is neither, so it can never enter that
  list by accident of wording. The deeper reason is the same one #579 was split out for: a refusal
  is `enqueue_text`'s `RejectFull` arm, which returns before `queue_seq.fetch_add`, so a refused
  delivery has no id and no queue entry. This classification requires the opposite — a payload that
  IS queued, holds an id, and cannot land. A delivery is in exactly one of the two states. The
  channels differ in kind too: #630 is a pull an orchestrator makes at session start, this is a push
  at the moment the bound elapses.

  **The one interaction that does exist runs through the notice's own delivery, not through the
  scan.** The notice is delivered to the ORCHESTRATOR's pane, so if that pane is at
  `QUEUE_MAX_PER_PANE` the notice is itself refused — and then correctly appears in #630's refused
  list, `from: "loomux"`, as the one loss that list was built to make visible. That is a
  complementary report, not a duplicate: the audit line records the *diagnosis* (written first and
  unconditionally, see #633 below), the refused row records that the *notice delivery* was lost, and
  the two are different facts about different objects. What it is worth knowing is that re-sending
  such a row hands over a diagnosis whose hold may have cleared since — stale, not wrong.

  **And it needs no cap headroom, unlike #615's pause-loss notice**, which is the sharpest way to
  see why. That one had to be admitted past the cap because it *reports the cap*, so on a full pane
  it was certain to be destroyed by the very condition it described. The equivalent loop here would
  need the stuck pane to be the orchestrator's own — and that is exactly the case `notify_queue`
  never enqueues at all: it parks for #578's inbox instead. So the one configuration in which the
  cap could eat this report is the one configuration in which the cap is not in the path. Same
  argument `HoldChannel::OrchestratorInbox` already makes: a full queue is a condition this reports,
  not an obstacle to reporting it.

  **Audit volume, which #630 asked the next writer to account for.** Its own note records that #624
  raised a busy group's audit volume (one line per queue notice), making `AUDIT_VIEW_LIMIT` easier
  to reach and NB1's `refused_window_truncated` more load-bearing than when it was written. This
  change adds to that too, and the bound is what makes it small: `notice-undeliverable` is
  **once per hold episode**, and an episode ends only when the pane accepts a delivery — so it is
  one line per stuck-pane incident, not one per poll, and a pane stuck for an hour still contributes
  exactly one. The parked `notice-suppressed` line it can produce on an orchestrator's own pane is
  bounded the same way. Where volume does push refusals out of the readable window, the honest
  reporting already exists and is untouched: `audit_log_windowed` reports the cut and
  `refused_window_truncated` carries it into the tool result. #630 also pinned that a parked
  orchestrator notice is not a refusal (`a_parked_orchestrator_notice_is_not_a_refusal`); this
  change adds a second producer of exactly that shape, and that test covers it unchanged.
- **#632 (multi-row mask gap) — closed by #638 while this was in review**, so this is no longer an
  open boundary but a rule this notice has to sit inside. It always did, by construction:
  `undeliverable_notice` renders one marker-led line for every `UndeliverableCause`, and it is the
  **seventh** constructor in `every_notice_that_can_reach_the_inbox_is_a_single_marker_led_line`,
  across `notify_queue`'s **eighth** call site — two different counts that an earlier draft of this
  bullet collapsed into one wrong number. The maskability assertion now runs through #638's shared
  `unmaskable_framing_rows` rather than a local `mask_loomux_notices(..).is_empty()`, so this notice
  is checked by the same expression as the multi-row producers #638 brought under the rule, and a
  regression names the rows that survived instead of only asserting that some did. The single-row
  property is asserted separately, because "every row is maskable" and "there is one row" are
  different claims and only the second is what `OrchNoticeInbox::park`'s `debug_assert` enforces.
- **#633 (silent `deliver_prompt` refusals).** The residual this change inherits rather than
  creates. `notify_queue`'s delivery to a live orchestrator can be refused with no audit line at all
  when that pane is dead or unbound, so the `notice-undeliverable` audit line is written **first and
  unconditionally** — the durable record does not depend on the notice landing. Closing #633 would
  make the delivery half observable too; nothing here needs it to be correct.

**Residual, stated.** A *second* notice joining a pane whose episode has already reported gets no
report of its own — one per episode is the bound, and an episode ends when the pane accepts a
delivery. That is a genuine limit rather than the one this section used to claim: the earlier
version said a notice arriving later in an episode never got a report at all and called that "the
conservative direction", which was a defect being described as a design (see the paragraph above).
And `PaneMidTurn` remains an inference from absence — see above for what the wording does and does
not claim.

## Kill-exit notices: recorded initiator, not inferred (#533-B)

**Problem.** `[loomux] agent X exited (kill or idle-timeout) — not a crash` prompted the
orchestrator, costing a full turn to acknowledge an event it had usually caused itself — it
called `kill_agent` a moment earlier, or the idle guardrail reaped a worker that had no task.
The roster (`list_agents`) already carries liveness, so the notice was re-stating durable state
through the most expensive channel available.

**Why `expected` could not be the signal.** The obvious hook was `on_pty_exit`'s `expected` flag.
It answers a different question: it is a property of the PTY (`PtyManager::kill` inserts the id
into `expected_exits`), so it says *loomux closed the pane*, never *who decided to*. A human
closing a pane, `end_group`'s teardown and the orchestrator's own `kill_agent` are
indistinguishable there. Routing on it would have demoted exits the orchestrator never initiated
— precisely the half this issue says must keep prompting.

**Approach.** The initiator is RECORDED, at the call site, before the pty is touched:
`AgentEntry::killed_by: Option<ExitInitiator>`, stamped by `kill_agent_as` (first-writer-wins —
whoever actually caused the exit got there first). `exit_notice_route` (pure) reads only that:
`Orchestrator` and `IdleTimeout` route to the audit log, everything else prompts. The demoted
notice is audited in full (`agent-exit-notice`, with `routed: "audit-only"`, the initiator, and
the complete notice text), so "the orchestrator reads it on demand" is a real path rather than a
euphemism for dropping it.

Stamping BEFORE the kill is load-bearing: `PtyManager::kill` can have the waiter thread inside
`on_pty_exit` before `kill_agent_as` returns, and a record written after the kill could lose that
race and misroute the notice to a prompt.

**A kill that kills nothing records nothing** (rev-13 F1). An agent between `spawn_agent`'s
registry insert and its bind has `pty_id: None`. The first shape of this change stamped
unconditionally and killed conditionally, so a `kill_agent` landing in that window returned
`Ok(())`, killed nothing, and left `killed_by` set — and since the stamp is first-writer-wins and
never cleared, EVERY later exit of that pane, including a genuine panic, routed `AuditOnly`. That
is the precise failure this half exists to prevent, so the no-pty case is now checked before both
the app handle and the stamp, and returns a truthful `Err` (the MCP handler passes it back to the
orchestrator) instead of a silent `Ok(())` for a no-op. Narrow to reach is not the same as
acceptable: a demotion path that can swallow a crash does not belong in the change that
introduces demotion. `a_kill_in_the_spawn_to_bind_window_records_no_initiator_and_a_later_crash_still_prompts`
drives that window specifically.

**How durable, exactly** — worth stating rather than leaving to the word "durable". The record
lives in the registry's own agent entry (process lifetime) and in the audit log (`agent-kill`
carries the initiator; the demoted notice is its own `agent-exit-notice` line). That is strictly
more lifetime than the decision needs: the routing decision happens on the pty waiter thread
milliseconds after the kill, in the same process, and a restart in that window leaves nothing to
route — the agent is gone either way and the roster loaded from disk says so. Persisting
`killed_by` into `agents.json`'s `AgentRecord` would add a field nothing reads. The point of
"recorded, not inferred" is *provenance*, not longevity: the fact comes from the code that caused
the exit, not from a downstream guess about it.

The idle reaper's OWN second notice ("respawn a worker when you have work for it") is demoted
too. Demoting only the exit notice would have left an idle kill costing exactly one turn anyway,
which is the outcome this issue exists to remove. That is safe *because* the reaper only ever
takes agents that are idle — no task in flight, nothing lost. **A future reaper mode that killed
agents WITH work would be a different event and must prompt**: give it its own `ExitInitiator`
variant routed to `Prompt` rather than reusing `IdleTimeout`. That instruction lives at the
routing switch, in `exit_notice_route`'s own doc, where the next person to add a variant will be
standing.

## Delivery failure actuation: stranded-prompt self-heal + attention badge (#496 PR-C)

**The guarantee.** *A delivery ends `Confirmed`, or a bounded self-heal fires, or a
human-visible attention badge is raised. No path ends silent-and-wedged.*

**Problem.** #451 gave a delivery three honest states and #445/#470 made a held payload
queued-never-destroyed — but nothing ever ACTED on `Failed`. Three suppressions compose into
a deadlock:

- For an **orchestrator-target** delivery both notices are suppressed by design
  (`should_notify_unconfirmed` / `should_notify_paste_held` require `!target_is_orchestrator`,
  to avoid the notice→delivery→notice loop). A failed delivery to the orchestrator is silent
  everywhere except the audit log.
- The recovery that *would* have fired — the next delivery's own pre-paste
  `flush_stranded_text` — needs a NEXT delivery. In an idle group there isn't one: the
  orchestrator is the thing that was supposed to act.
- Until PR-A (#499), the phantom `user_input_ms` stamp also suppressed the flush and the
  submit retries outright.

What was left was a human noticing a stuck prompt and pressing Enter by hand. Observed on
Claude panes as well as copilot ones — the root cause is CLI-agnostic (#496's scope
correction); whether a CLI *drops* or *queues* Enter changes how often a prompt strands, not
what happens afterwards.

**Trigger — the ledger first, the pane second.** Actuation hangs off the one point a delivery
is ever declared `Failed`: `late_monitor_tick`'s `DeclareFailed` (already 60s output-quiet,
no question on screen, nothing confirmed). `stranded_selfheal_action` is the pure decision,
and its FIRST input is the durable artifact — this pane's recorded `DeliveryOutcome` is still
this delivery's and still unconfirmed. A pane reading can never override what the ledger says
about whether a delivery is outstanding. Then, in order: `human_typed_since` (never
merge-submit over human-typed content, #81/#84's rule), `question_on_screen` (#420's guard),
`box_holds_paste` (Tier 1's own signal — is our text still identifiably there), and last the
heal budget. Precedence is pure and table-tested precisely because reordering it is how this
would turn into a bug that types Enter over someone's half-written line.

**The heal goes THROUGH the queue, never around it.** The re-submit is admitted as a
`StrandedSubmit` marker at the FRONT of the pane's queue (`admit_stranded_selfheal` →
`enqueue_stranded_front`) and pressed by the drainer via `drain_stranded_submit` →
`flush_stranded_text` — the same single-consumer path #470 made the only way anything reaches
a pane, and the same marker `AbortedPreEnter` already produces. Three consequences, all
deliberate:

- **Front, not back**: the stranded text is physically in the box already, so anything queued
  behind it must not paste on top of it. Same ordering rule `AbortedPreEnter` follows.
- **Not a new delivery**: nothing is re-pasted and no new payload is admitted — a marker is a
  single Enter. A raw `write_bytes` from the monitor thread would have been simpler and
  wrong: it races the drainer mid-paste and re-opens exactly the ordering hole #470 closed.
- **Guardrails re-used, not re-implemented**: `flush_stranded_text` re-derives
  `human_typed_since` from the ledger and re-reads the live question state at the instant of
  the press. The decision above is the *trigger* gate; it is never the last word on whether
  the Enter is safe.

**Admission gate: never cut in front of a drainer that owns an entry**
(`stranded_admission_gate`). Pushing to the FRONT is safe only while no drainer owns the front
entry. A drainer inside `deliver_now` has already peeked its entry and finishes by calling
`pop_front_dequeued(that id)`, which pops ONLY on an id match — a marker slipped in front makes
that pop match nothing and leaves an already-delivered entry queued for a **second, duplicate
delivery**. Pre-#496 nothing could reach this (the front door pushes to the BACK; the drainer's
own `AbortedPreEnter` marker is pushed *after* it pops), and the self-heal must not become the
first thing that does.

*The gate must be atomic with the push, and first shipped in this PR it wasn't* (review rev-47
B1). The original shape peeked `queues` and released, read `queue_draining` and released, then
re-took `queues` to push: check-then-act across three scopes. A drainer registering in that gap
re-opened the hazard exactly — gate sees none, a delivery is admitted and its drainer
spawns/peeks the Text entry, the marker lands in front, and the completing pop matches nothing.
Not an exotic window either: the delivery most likely to arrive at that instant is the idle
tick's nudge to the same wedged pane, and the idle tick and `DeclareFailed` key off the same
quiet condition — correlated, not independent. **The fix fuses the check and the push into one
critical section on `queues`, reading `queue_draining` while holding it** (lock order `queues` →
`queue_draining`, exactly `commit_exit`'s, so no new deadlock edge). That is sufficient because
any drainer that could ever peek this front must register *before* it peeks and must take
`queues` *to* peek: relative to the fused section it is either already registered (→ decline) or
it peeks strictly after the push (→ finds the marker at the front, the safe case — it drains the
submit first and pops it by a matching id). `queue.rs`'s `stranded_admission_property` proves
this over every interleaving, with the unfused variant as its mutation control, in the same
shape #470 B1's own `unified_admission_property` uses one layer up.

Nothing is lost by declining — a live drainer means a delivery is already queued for this pane,
and that delivery's own pre-paste `flush_stranded_text` presses exactly the Enter the heal
wanted pressed. The self-heal exists for the case where no such next delivery exists (an idle
group), which is precisely when no drainer is running. The badge is raised either way: the human
is told regardless of which mechanism does the pressing.

**Bounded.** `STRANDED_SELFHEAL_MAX_HEALS = 1` per stranded delivery, counted in the monitor.
The bound is also structural today (`late_monitor_tick` returns `DeclareFailed` at most once
per monitor — every later tick sees `already_failed`), but that is the *caller's* precedence:
counting explicitly means the cap survives an edit to it and is directly testable. A second,
independent bound sits at admission: `admit_stranded_selfheal` refuses to stack a marker
behind one already at the queue's front, so no caller can produce two un-fired Enters, and a
refused admission does not burn budget it never spent. Every attempt, refusal and clear is
audited (`stranded-selfheal-submit`, `stranded-selfheal-skipped`, `stranded-attention`,
`stranded-cleared`).

**Badge — chrome, never a resize.** The loud surface reuses the existing #40/#6 attention
event (`orch-attention`) with one new reason, `stranded`, ranked directly under `blocked` and
above `waiting`: a wedged pane will not un-wedge itself, whereas a `waiting` pane is asking a
question it is happy to keep asking. It renders on the pane-header chip and the dock chip that
already exist — overlay chrome floating over the terminal, no PTY resize (hard constraint 1),
no new notification channel. The badge is raised for a self-heal too, not only for the blocked
cases: a delivery that needed healing already burned its whole confirm window plus
`PENDING_IDLE_QUIET`, and the human is entitled to know their group wedged even when loomux
recovers it. `StrandedBlocker` carries WHY into the detail text, so the badge always says what
the human has to clear rather than leaving them to work it out from the pane.

**The badge stays honest while it is up** (rev-47 NB1). An admitted marker is not a fired one:
`flush_stranded_text` re-decides at press time and can decline indefinitely — most plausibly
when the human starts typing *after* the marker was admitted, which the trigger-time decision
could not have seen. Safety holds (their text is never submitted over), but a badge reading
"loomux is re-sending it" while the press will never fire is a claim the code doesn't honor. So
each `KeepWaiting` tick with a badge up re-runs the SAME pure decision against live state and
re-words the note when a real blocker has appeared — one bounded tail read, shared with the
clear-check below, and only taken when a badge is actually raised. It re-marks once (the note
stops being `blocker: None`), so this is a state transition in the audit log, not per-tick spam.

**Clearing — on evidence, and it must be able to clear.** A badge that never comes down trains
the human to ignore it. Three clear paths, all ledger- or evidence-driven: the monitor's
`Confirm` (the prompt landed after all); `Superseded` where the newer recorded outcome is
confirmed (which includes our own heal, since `drain_stranded_submit` records a successful
press as a fresh confirmed outcome); and a present→absent transition of our text in the box
(`text-left-the-box`) — the case where a HUMAN rescues the pane on a CLI that produces no
`promptsubmit` hook, where waiting for `Confirm` would leave the badge up until the monitor's
4h cap. Only a genuine transition clears, so a `NotHolding` badge (raised because the text was
already gone) is never un-raised by the same reading that raised it. The map is otherwise
latched — nothing about a wedged pane changes on its own — and pruned in `attention_tick` when
the agent stops running.

**Honest residual.** `drain_stranded_submit` records a successful *write* as confirmed; it
does not re-verify that the CLI accepted the healed Enter (pre-existing behavior, unchanged
here). If a CLI drops the healed Enter too, the badge clears on the optimistic record and the
prompt is recovered only by the next delivery's own flush. Making the heal's landing
independently verifiable belongs with PR-D's processing-evidence work, not here — this PR does
not claim that guarantee.

## Lost spawn-time kickoff: re-delivery, not just a badge (#517)

> **Read with #585.** Everything below describes this recovery correctly, but it did not run:
> a `return` in #522's idle-pane arm sits above the test that gates it, and the audit shows
> **zero** re-deliveries and zero declines across the whole recorded history. Whether this
> feature is reachable at all is settled in "An empty box is two panes, and silence needs
> evidence (#585)" below — the recovery's own logic, budget, and declines are unchanged by it.

**Problem.** A fresh spawn's kickoff brief kept going missing: five instances on one day
across v1.0.0, then two of three fresh spawns on v1.1.0-beta — each agent came up idle
reporting "no task brief received" and needed a manual `send_prompt` from the orchestrator.
The third beta spawn received its brief normally, which is what makes this a race with CLI
readiness rather than an ordering bug.

**What it is NOT.** The issue's own hypothesis was that the spawn path bypasses admission.
It does not. `spawn_agent_ex` delivers the kickoff via `deliver_prompt(…,
Delivery::FreshKickoff)` — the same unified front door #470 made the only way anything reaches
a pane, with the same ordered admission, the same coalesce, and the same three-state
confirmation (#451). The kickoff was never outside the machinery. Only its *failure* was.

**The two defects, in the order they fire.**

*1 — the boot wait trusted a signal that freezes.* `deliver_now` holds a kickoff paste until
the CLI has painted and its output has been stable for `READY_QUIET`. It measured stability
from the length of the output ring — but the ring is capped at `OUTPUT_RING_CAP` (256 KB), and
`OutputBuf`'s own doc says why its monotonic `total` counter exists at all: past the cap,
"lengths stop changing". A CLI whose boot paint exceeds 256 KB therefore froze the length at
the moment of saturation, the quiet bar was met `READY_QUIET` later while the CLI was still
booting, and loomux pasted into a stdin reader that had not attached yet. Below saturation the
two readings are identical — which is exactly why this read as intermittent, and why one spawn
in three was fine. The fix is to sample `output_total`, the counter every other progress loop
in the file already reads (`await_cli_ready`, extracted so the loop is testable with its clock
and sampler injected). `ready_observed` is now audited: "we pasted into a CLI we never saw
become ready" is the single most useful fact about a kickoff that then fails to confirm, and
both exits from that loop used to look identical in the record.

*2 — an eaten paste had no recovery, only a badge.* #496 PR-C (above) rescues a delivery whose
text IS in the box with its Enter withheld. A swallowed paste is the other shape: nothing ever
reaches the box, so `stranded_selfheal_action` correctly returns
`Attention(StrandedBlocker::NotHolding)` — pressing Enter into a pane whose state loomux
cannot account for would be a guess — and before this the story ended there. For a mid-session
prompt that is the right answer: the sender is still around. For a fresh spawn's brief it is
not, because that brief exists nowhere else and nothing will re-send it.

**The fix: re-deliver, through the same front door.** Inside that one `NotHolding` outcome, and
only for `Delivery::FreshKickoff`, the late monitor consults `kickoff_recovery_action` and — if
it says so — re-admits the brief via `redeliver_lost_kickoff`, which calls `enqueue_text` with
its own `EnqueueReason::KickoffRecovery`. Not a write from the monitor thread: that would race
the drainer mid-paste and re-open the ordering hole #470 closed, exactly as PR-C's own note
says about its marker. Going through the front door means the re-delivery inherits ordering,
every paste guard, and the same three-state confirmation — so a re-delivery that also fails is
loud rather than a second silent loss.

**Why this cannot double-deliver a brief that landed** — the property the live counter-example
demands, since a duplicate brief is a real cost. Four independent layers:

1. A landed kickoff never reaches `DeclareFailed` at all: `late_monitor_tick` returns `Confirm`
   on the `promptsubmit` hook record (#112, installed for every spawned agent on both CLIs at
   spawn) and the monitor exits.
2. `ledger_outstanding` — the durable artifact, checked before any pane reading, exactly as
   `stranded_selfheal_action` checks it first.
3. `output_since_submit` against `KICKOFF_TURN_EVIDENCE_BYTES` — a kickoff that landed makes
   the agent run its whole first turn; one that was eaten leaves the pane as boot left it. This
   is the live discriminator (two spawns silent, one working) expressed as a rule. The bar is
   set high on purpose: too high only declines a recovery that was needed, falling back to the
   pre-#517 badge; too low re-sends a brief that landed. What makes 4 KB enough is not the
   brief's own size (real briefs run ~1–2 KB — review F3 removed that claim from the code
   comment) but the window: the monitor only looks after `PENDING_IDLE_QUIET` of *total*
   silence, so a turn that happened has painted kilobytes by then. When the delivery pasted
   blind (`ready_observed: false`), growth is reported as `OutputUnattributable` rather than
   `TurnStarted` — same conservative decline, but the audit never claims a turn when the boot
   paint it timed out on explains the bytes just as well (review F5). Bias toward the reversible mistake.
4. `queue::admit`'s byte-identical coalesce, reported out of the same critical section that
   decided it (`AdmitOutcome::coalesced`) so a brief still waiting to drain can never occupy
   two slots — and a collapsed attempt is reported as NOT sent, so it does not burn the budget
   for a send that never happened.

Plus a budget of one (`KICKOFF_REDELIVERY_MAX`), for the same reason `STRANDED_SELFHEAL_MAX_HEALS`
is one.

**Scope, deliberately narrow.** `Delivery::recovers_lost_kickoff()` is true for `FreshKickoff`
alone — narrower than `wait_ready`/`confirms_autopilot_dialog`, which both include a resume.
A `ResumeKickoff` payload is a re-sync notice re-derivable from durable state on the next
resume, and a `MidSession` prompt has a sender still around; both keep the pre-#517
badge-and-stop behavior untouched. Every other `StrandedAction` is likewise untouched — this
lives strictly inside `NotHolding`.

**The re-delivery gets the boot wait too** (review F4). The recovery originally nudged the
drainer with no kickoff treatment, so the re-delivered brief pasted with `wait_ready: false` —
the one place this feature could reproduce the bug it exists to fix, since a CLI whose stdin
reader still had not attached would eat the re-delivery as well. That is *unlikely* (the monitor
only declares failure after `READY_MAX_WAIT` plus `PENDING_IDLE_QUIET`, by which point a healthy
CLI has long been reading) but not impossible by construction, and unlikely is not the bar for
the ghost of the original defect. `redelivery_treatment` now gives it the same wait, at a cost of
`READY_MIN_WAIT` (1.5s) on a pane that is already ready. The other two flags are deliberately
*not* copied: `confirm_autopilot: false`, because copilot's consent dialog answers the FIRST
submit and re-arming that watcher would put a stray Enter into the pane being recovered; and
`fresh_kickoff: false`, which is what bounds the whole feature at one recovery — a re-delivery
that is itself eaten degrades to the loud pre-#517 badge rather than triggering another, so there
is no re-send loop even with every budget check removed. The three are a named struct, not a
`(bool, bool, bool)`: same type, opposite meanings, one transposed edit from arming the autopilot
watcher or unbounding the feature. `was_first` gates the whole thing, mirroring
`deliver_prompt`'s own rule that kickoff treatment belongs to the entry the drainer's first pass
actually picks up.

**Badge honesty.** `actuate_stranded` raises `NotHolding` ("its text is gone — check the pane")
before the recovery runs. Once a re-delivery is admitted that is no longer the whole truth, so
the note is re-worded to the in-flight form (`blocker: None`, "loomux is re-sending it") — the
same wording an in-flight self-heal uses, and it clears on the monitor's normal evidence paths.
The human is neither told to act nor left thinking nothing is happening.

That re-wording has to *survive*, which the first cut of it did not (review F2). Every later
`KeepWaiting` tick re-runs the rev-47 NB1 live check, and for an eaten kickoff that check reads
`NotHolding` **permanently** — the text is gone, which is precisely why a recovery is queued —
so the badge flipped straight back to "check the pane" on the next 5 s tick and stayed there until
the re-delivery drained. `stranded_reword` is now the single decision for what a live re-check may
say: it spares `Exhausted` (the pre-existing NB1 rule — the budget arm firing on our own queued
marker *is* the in-flight state) and spares `NotHolding` while `redeliveries_used > 0`. Everything
a human can actually clear — `HumanInput`, `Question`, `QueueFull` — still outranks an in-flight
recovery and is said out loud. The shape of the bug is worth naming: a badge claiming "your
problem" about loomux's own pending work is the same honesty failure the NB1 re-check exists to
prevent, pointed the other way.

**Interaction with #454 (supersede at a re-send's START).** These two land together and compose
exactly the right way. #454 makes `deliver_now` call `record_inflight_delivery` *before* its
confirm window, so a new delivery claims the pane the moment it presses Enter rather than when it
finishes. The re-delivery admitted here is an ordinary delivery, so when the drainer runs it the
old monitor's very next `observe_ledger` reads `superseded` and it exits without writing or
notifying — the recovery cannot be raced by the monitor that triggered it. That sequence is now
composed in a test rather than argued here (review F1):
`redeliver_lost_kickoff` → `record_inflight_delivery` (as the drain will, with a fresh
`submit_sent_ms`) → `observe_ledger(original).superseded` → `late_monitor_tick == Superseded`,
plus the new delivery reading as outstanding under its own identity so nothing is left unwatched.
In the other direction,
the `ledger.outstanding` this decision reads is the SAME atomic `LedgerView` the self-heal decision
above judged from, so the two decisions taken on one tick can never disagree about whether the
delivery is still outstanding.

**Honest residual.** The recovery is driven from `run_late_confirmation_monitor`, which needs a
live `AppHandle` and so cannot be driven headless; the tests compose the same real functions in
the monitor's own order with only the pane readings faked, which is the same bar #496 PR-C's
tests meet. The wiring line itself (the `NotHolding` arm calling `kickoff_recovery_action`) is
covered by inspection plus the mutation control, not by an end-to-end driver. Independently
verifying that a re-delivered brief was *processed* — as opposed to submitted — remains PR-D's
processing-evidence work; this does not claim that guarantee either.
## The human-input signal: a sourced bit and a bounded block (#518 / #522)

**Problem.** On v1.1.0-beta a copilot orchestrator's prompt sat unsubmitted behind a badge
claiming a human was typing in the pane. Nobody was. #510's self-heal declined — *correctly*,
per its own rule — so a false signal became an indefinite badged hold that only a physical
Enter cleared.

**First, the premise correction.** The issue supposed the delivery hold reads a *different*
signal from the one #499 gated. It does not. There is exactly one writer of the
keystroke-recency clock (`PtyManager::note_user_input`) and every delivery-path consumer reads
it back through `last_user_input_ms`. #499 shipped in the beta. So the defect is not an
ungated consumer; it is two other things.

**The consumers, enumerated** (an enumeration is a claim of completeness):

| Consumer | Bounded before #518? |
| --- | --- |
| Pre-paste and pre-Enter typing holds → `HeldReason::Typing` | yes — `USER_QUIET_MAX_HOLD` |
| `input_pending` → `HeldReason::BoxOccupied` | yes — `HUMAN_INPUT_HOLD_MAX` |
| Idle tick's quiet clock | yes — #500 |
| `human_typed_since` → suppresses `flush_stranded_text` | **no** |
| `human_typed_since` in `drain_stranded_submit` (the press) | **no** |
| Submit-retry suppression | no, but unreachable inside the retry window |
| `stranded_selfheal_action` → `StrandedBlocker::HumanInput` | **no — the reported symptom** |
| `tier1_trusted` — Tier-1 confirmation | n/a, scoped to the delivery |

Each unbounded one inlined `last_user_input_ms > submit_sent_ms` — `tier1_trusted`'s inverse.
**That expression is a latch.** One stamp after our submit pins it true for the life of the
delivery *and* its late monitor, whose `KeepWaiting` arm re-asserts the badge off it every
poll for up to `LATE_MONITOR_MAX_LIFETIME` (four hours). That is what makes a wrong signal an
*indefinite* one rather than merely a wrong one.

**Why the signal was wrong: an open set, not a missing case.** #499 classifies by byte shape —
skip anything parsing as CSI/OSC/DCS. That covers every reply shape #179 catalogued, but the
set is open, and #496's own plan §7 closed with *"which copilot emission recurs mid-session"*
unanswered; the `phantom-input-gated` breadcrumb exists because we do not know. This repo
already settled the argument in the other half of the same problem: #440 B2-R hit it for the
frontend's `firstInputMs` and deliberately did **not** answer it with a better `onData` filter
(bracketed paste itself starts with ESC, so shape-filtering misfires on real pastes). It took
the signal from `term.onKey` and the two `term.paste()` sites — *a structural guarantee, not a
pattern match against an open set*.

**Fix 1 — source the bit.** `src/humanorigin.ts` is a short-lived latch marked by exactly the
three call sites `markFirstInput()` already trusts. xterm fires `onKey` synchronously
immediately before the `onData` it produced, and `term.paste()` triggers its `onData`
synchronously from the call, so a human write reads the mark still open; a query reply is
emitted while xterm parses program output — a different turn — and reads it closed. The flag
is captured at `write()` time and carried *with* the data through the ordered writer, because
the IPC send lands turns later and because chunking must not make chunks 2..n look
foreign. `write_pty` gains `human: Option<bool>`; absent means `true`, so the command's
existing shape keeps its meaning and an unstated origin degrades to the pre-#518 behaviour —
the fail-safe direction, since believing a human typed only ever makes delivery hold *more*.
The bit is **ANDed** with #499's shape gate, never substituted for it: #496 PR-A's deliberate
Neutral/zero-delta tradeoff (an arrow key does not defer anything) is untouched.

**Fix 1a — the two human paths `onKey` never sees, and the scheduling trap in them.** Read
from `@xterm/xterm` 6.0.0's own `lib/xterm.js` rather than assumed:

```
_finalizeComposition(e){...this._isSendingComposition=!0,setTimeout((()=>{
 ...t.length>0&&this._coreService.triggerDataEvent(t,!0)}),0)}
_inputEvent(e){if(e.data&&"insertText"===e.inputType&&(!e.composed||
 !this._keyDownSeen)...{...this.coreService.triggerDataEvent(t,!0)...}}
```

An **IME commit** is sent from a `setTimeout(…, 0)` — a later task than the `compositionend`
that caused it. The **`insertText`** path (dead keys, accents, soft keyboards) sends
synchronously with no key event at all. Both are still structural signals — they are DOM
events on the terminal's own textarea, and xterm never routes a query auto-reply through the
textarea; it calls `triggerDataEvent` directly — so `pane.ts` marks from them too, in the
CAPTURE phase on `termEl`, an *ancestor* of the textarea, since ancestor-capture listeners run
before any listener on the target itself.

That ancestor-capture choice is required for the synchronous path, and it is exactly what
makes the deferred one subtle. **The first cut of this got it backwards and shipped**: it
reasoned that a close scheduled from our listener would be registered *after* xterm's send, so
one `setTimeout(…, 0)` would suffice. Running first means our timer is queued *first*, and
equal-delay timers fire in registration order — so the close beat the send and **every IME
commit read non-human**, silently removing the protection from exactly the users most likely to
have an unfinished composition sitting in the box. Caught in review (#528 B1), reproduced
against the real module.

`markDeferred()` therefore closes over **two** timer hops, scheduling the second only once the
first has run. That lands it in a strictly later round than any send registered during the
original dispatch, whichever timer was queued first — the ordering dependency is removed rather
than inverted, which matters because the order is a consequence of DOM capture semantics that a
future listener change could flip back. The window is two timer rounds (tens of milliseconds on
a coarse-resolution host, ~2ms in a foreground Chromium); if it is ever mis-sized it fails
toward "human", i.e. the pre-#518 behaviour of holding more and never clobbering.

The lesson worth carrying: a manual-queue unit test **cannot** see this class of bug. The
original tests modelled the deferred window's shape faithfully and passed, because none of them
registered a competing send. `humanorigin.test.ts` now pins the ordering with real timers and a
racing timer registered after the mark, which is what the review's own repro did.

Occupancy (`input_box_len`) is deliberately *not* gated on the origin bit. An auto-reply
already contributes a zero delta, so there is nothing to add — and under-counting occupancy is
the one direction `box_occupancy_delta` commits to never taking, because reading an occupied
box as empty is the clobber #111 exists to prevent. The bound below reads `input_pending` as
its positive evidence, so that counter must stay governed by the conservative rule alone.

**Fix 2 — bound the block.** One pure `human_input_block(last_user_input_ms, submit_sent_ms,
box_pending, now_ms, bound_ms)` replaces every inline derivation, returning three-way
(`None` / `Blocked` / `BoundedOut`) so a release is auditable rather than silent
(`human-input-block-released`). `HUMAN_INPUT_BLOCK_BOUND_MS` is ten minutes, sized against the
delivery machinery's own longest legitimate window — `REINJECT_CONFIRM_TIMEOUT_MS` (5 min) is
already documented as comfortably longer than the entire worst-case hold chain — so the bound
cannot elapse inside a live delivery.

This is #500's rule applied a third time: *a suppression driven by a fallible signal must be
BOUNDED, because nothing else will ever clear it.* It differs from #500's clamp in one
deliberate way. #500 is classification-blind and releases on elapsed time alone. This one
releases only on **positive evidence**: `box_pending` outranks the bound, so the block can
never time out while a single human-typed character is outstanding. That ordering is the
safety contract, and it is table-tested precisely because reordering it is how this becomes a
bug that presses Enter over someone's half-written line. `box_occupancy_delta`'s own bias
("never UNDER-count real occupancy") is what makes a `false` reading trustworthy here.

Nothing about #510/#451/#420 is weakened. Once the block releases, precedence carries the
decision into the **existing** `box_holds_paste` arm of `stranded_selfheal_action` — which is
already exactly "deliver if the box is unchanged": our own text still at the tail means there
is nothing of the human's there to merge with, and any other reading still lands on a badge
(`NotHolding`, or since #559 `Unverifiable` where the box could not be read at all), never a
blind Enter. `drain_stranded_submit` re-derives through the same
helper rather than forming a second opinion, because rev-47 NB1 is the failure mode where a
marker admitted under one rule and pressed under another never fires at all.

**Why no per-group guardrail.** #500's bound got one because it is classification-blind, so a
group whose humans really do sit typing for twenty minutes has a legitimate reason to want it
longer. This one cannot release without evidence that there is nothing to clobber, so there is
no workflow for which a different value is more correct — and a knob with no correct second
setting only ever gets set wrong.

**#522 — an idle pane is not a stranded one.** The same monitor announced *"the prompt may be
sitting unsubmitted in its pane; get_output it and re-send if needed"* about a worker that had
finished its turn and gone idle at the CLI's rest prompt. The monitor knew only that no
`PromptSubmit` hook record had appeared — which copilot never produces at all and a claude
pane can miss — and read that absence as a strand. The cost is not noise: each one sends the
orchestrator to `get_output` (the #520 token flood on an animated pane) and tempts a duplicate
re-send, the loop #451/#510 exist to prevent.

`unconfirmed_disposition(reading, box_pending)` samples structural state *before* the
notice, reusing the seams that already exist rather than inferring anything from output bytes.
"Sitting unsubmitted" means our text is still identifiably at the box's tail, or the human's
characters are outstanding; neither means the pane is done. **#559 made the first input a
`BoxReading` rather than a bool**, because the quiet path is only ever justified by a box loomux
actually *read*: a `false` produced by a tail shorter than the paste it was looking for
(`BoxReading::Unverifiable`) now notifies, and only an observed `NotHolding` reaches
`IdleAuditOnly`. The third condition #522 names —
CLI at rest — is a *precondition* of reaching the decision rather than an input to it:
`late_monitor_tick` returns `DeclareFailed` only when `quiet_long_enough`, and a pane mid-turn
keeps `output_total` creeping (spinner and statusline frames included, #480), so it never gets
that far. Taking it as a parameter would let a caller assert a state the surrounding code
cannot produce.

It deliberately does **not** mark the delivery confirmed. An idle reading is good enough to
withhold an alarm, not to assert a landing; leaving the ledger unconfirmed keeps every
downstream behaviour identical, including the residual `should_flush_before_paste` already
blesses ("A false 'unconfirmed' here is safe: the flush Enter lands on an already empty box
and is a no-op"). It also holds nothing across a restart: it reads live pane state, and a
restart destroys the pty, so there is no delivery left to judge.

**No coalescing mechanism, deliberately** (#528 review N1) — *superseded by #539, see below.*
The burst that motivated this — 17 false alarms in one day — was collapsed *at the source*, not
by de-duplicating notices downstream: idle panes produce `delivery-unconfirmed-idle-pane` audit
records and no actionable notice at all, so a burst of false alarms collapses to zero. A burst
of GENUINE strands still notified once per pane, and that was the intended behaviour rather than
an oversight: each names a different pane the orchestrator must actually do something about, and
collapsing them would trade a known false-positive problem for an unknown false-negative one.
The open questions were named at the time — *which pane's identity survives the merge? what
re-arms it?* — and #539 answers them (per-pane bucket keyed by `(group, agent)`; the first alarm
after a flush re-arms), which is why the decision moved rather than being reversed silently.

**Test seams.** Two, both back-dating the *real* fields the real readers read rather than
mocking a clock, so the tests drive the production path with production `now_ms()` and the
shipped bound: `PtyManager::set_user_input_ms_for_test` and
`record_stranded_outcome_at_for_test`. Without them the only way to reach a stale block is to
sleep out ten minutes.

**Honest residual.** The copilot-specific OSC 10/11/4 shapes still cannot be reproduced
headless — `@xterm/headless` has no renderer/theme service to answer them from, and no `onKey`
at all (which is itself why `onKey` is safe). `test/xterm-humaninput.test.ts` pins the general
form via Device Attributes; the copilot shapes remain a live hand-check. And the origin bit
is only as good as the frontend's own vectors: a future input path that reaches `writePty`
without going through `markFirstInput()` would arrive marked non-human. That is the
fail-*unsafe* direction for this one signal, which is why both marks hang off that single
function rather than being re-derived at each call site.

**And that enumeration is now executable (#570).** "One function, so a vector can only be
added in one place" is a convention, not a mechanism — it was arrived at by hand twice (#440
B2-R, then #518) and lived nowhere a compiler or a test could read it, so the next path to skip
it would have degraded the bit silently. `test/inputvectors.test.ts` is the tripwire: a
data-driven scan of `src/` that pins five call shapes — an explicit `.paste()`, an `onKey`
handler, `writePty` going through the ordered writer, `writePty` carrying its origin argument,
and `onData` consulting the latch — and names the offending `file:line` and what is unsafe
about it when one fires. Comments and literals are stripped first, because most occurrences of
these shapes in `src/` are the prose *about* them, including this subsystem's own design
argument in `humanorigin.ts`. It is a tripwire and not a proof: it knows today's shapes, and a
genuinely novel vector (a new xterm API, a raw `invoke("write_pty")`) is not in its table — so
the file's last test asserts every rule still matches real code, since a rule that has drifted
away from the code reports green about a file it no longer reads. No product code changed for
it: the one seam that could carry a debug assertion, `createOrderedWriter`'s
`write(data, human = true)`, defaults to human deliberately (an un-updated call site keeps its
pre-#518 meaning), so an assertion there would fire on the documented fail-safe rather than on
the defect.

## An empty box is two panes, and silence needs evidence (#585)

**Problem.** `BoxReading::NotHolding` on a quiet pane — our pasted text is not at the box's
tail, and no human characters are outstanding — has two causes, and the reading cannot tell
them apart:

- the CLI **consumed** our text, ran its turn, and came back to rest. #522's case: an alarm
  here is noise, and an expensive kind (it sends the orchestrator to `get_output`, the #520
  token flood, and tempts the duplicate re-send #451/#510 exist to prevent).
- the paste was **never accepted**: a blind paste into a CLI whose stdin reader had not
  attached, or an Enter that landed while the CLI was busy and was dropped. The box is equally
  empty and the delivery is equally absent — except this one is a *lost message*.

#522 modelled only the first and returned from the `DeclareFailed` arm on it. That `return`
sits roughly ninety lines **above** the `NotHolding` test that gates #517's kickoff recovery,
so the recovery was reachable only when `NotHolding` coincided with a human's half-typed line
(`box_pending`) or with an `Unverifiable` reading — that is, on a coincidence unrelated to
whether a kickoff had been lost.

**It never fired.** Across this project's entire recorded audit history —
both `audit.jsonl` generations of the group that filed #585 — `kickoff_recovery_action`
produced **zero** `kickoff-redelivered` rows and **zero** `kickoff-redelivery-skipped` rows,
against 13 `delivery-unconfirmed-idle-pane` records and 3 fresh kickoffs whose submit was never
confirmed. Not "rarely": never, and not even a decline naming a reason. The #517/#526 feature
was dead code from the day it shipped, and nothing in the log said so — the silence it was
supposed to replace is what wrote the record.

The live cost, from that same session: one reviewer agent sat idle for **11 minutes** after a
lost kickoff, rescued only incidentally by an unrelated later prompt; two workers had a
follow-up brief eaten with no badge, no notice, and no recovery. 420 of 2070 deliveries
(20.3%) failed submit-confirm in that session, and 3 of 12 fresh kickoffs (25%).

This is the same shape #559 fixed for `Unverifiable`, and #559 fixed only the lower half of
it: it widened the recovery's own test without noticing that the early return above still
swallowed `NotHolding` + idle, which is the common case and the one #517 was written for.

**The fix: the bar that already governs the riskier decision now governs the silence.**
`unconfirmed_disposition` takes a third input, `turn_evidence` — did the pane produce
`KICKOFF_TURN_EVIDENCE_BYTES` (4 KiB) of output since this delivery's own Enter? The
`NotHolding`/no-human cell splits on it:

| `reading` | `box_pending` | `turn_evidence` | result |
| --- | --- | --- | --- |
| `Holds` | any | any | `Notify` |
| `Unverifiable` | any | any | `Notify` (#559, untouched) |
| `NotHolding` | `true` | any | `Notify` |
| `NotHolding` | `false` | `true` | `IdleAuditOnly` (#522's silence, unchanged) |
| `NotHolding` | `false` | `false` | **`EatenNotify`** (#585) |

`EatenNotify` does not return early. It falls through to the notice, the attention badge, and
`kickoff_recovery_action` — which stays bounded at `KICKOFF_REDELIVERY_MAX = 1` and keeps every
one of its declines, `NotAKickoff` included.

**Why this is not a weakening of #526's evidence bar.** It is the *same* bar, and it was
already trusted — ninety lines below — to gate the far more dangerous decision of re-sending
text into a live pane. Authorising a *notice* is strictly safer than authorising a
*re-delivery*. So the argument runs one way only: if the bar is good enough to fire a
re-delivery, it is good enough to fire a notice. Nothing here loosens the re-delivery
condition, adds a blind re-send, or widens who may be re-delivered to — a lost non-kickoff
still declines `NotAKickoff` and is escalated to its sender rather than re-sent, exactly as
that variant's doc argues. The bar is not new, not lowered, and not duplicated; it is newly
*applied* to the path that was bypassing it. Both call sites now read one binding computed once
per tick, so the two decisions cannot disagree about the same question — the discipline the
shared `ledger` observation already follows.

**What this does not re-open.** #522's flood stays suppressed where #522 aimed it. A worker
that finished a turn and went idle has turn evidence by construction — the monitor only looks
after `PENDING_IDLE_QUIET` of total silence, so a turn that happened has long since painted
kilobytes — and still takes `IdleAuditOnly`. The only deliveries that newly speak up are ones
where the pane produced *less than a turn's output* since our Enter **and** the box no longer
holds our text, which is not a finished pane under any reading.

**Bounded with escalation, and the escalation had to be honest.** The pre-#585 notice tells the
orchestrator the prompt "may be sitting unsubmitted in its pane". For an eaten delivery that is
false: we looked, and the text is gone. Acting on it sends the orchestrator to `get_output`,
where it sees an idle pane and reasonably concludes nothing is wrong. That is not hypothetical
— it is exactly how #585's two live losses were misread, and the pre-emptive re-sends the
misreading invites are the #455 duplicate-kickoff class. So an eaten delivery gets its own
wording (`delivery_eaten_notice`), which states what was observed and names the idle pane the
orchestrator is about to see, so an idle pane is not mistaken for a refutation.
`.loomux/lessons.md`, "a claim is a deliverable."

**The audit now records the evidence with the silence.** `delivery-unconfirmed-idle-pane`
carries `output_since_submit` and `turn_evidence_bytes`, and the eaten case gets its own
`delivery-eaten` action plus an `eaten` discriminator on `delivery-unconfirmed-notice`.
Reconstructing #585 required reading the source to find out what a silent row had decided;
"how often is a delivery actually eaten?" is now a log question rather than a code question.

**Composition with #539.** PR #588 adds an `agent_acted` input (the MCP activity clock) to the
`NotHolding`/`box_pending: true` cell. It is a strictly stronger signal than output bytes — it
proves the agent's own code path ran — but it governs a *different* cell and does not reach
this one, which is why the two are additive rather than duplicates. Whichever lands second, they
compose as evidence-OR: any independent sign that the agent acted on our delivery justifies
silence, and the absence of all of them is what must speak. Whoever rebases second should merge
the two precedence tables into one rather than leaving two `NotHolding` arms to drift.

**The precedence is now a value, because that is what regressed.** #585 was not a wrong
decision. Every decision involved — `unconfirmed_disposition`, `stranded_selfheal_action`,
`kickoff_recovery_action` — was individually correct, individually tested, and green. What
failed was the *order* they ran in, and the order existed only as control flow inside
`late_monitor`, which no test can execute (it needs a live `AppHandle` and a real
`PtyManager`).

#517's own wiring test shows how that hides: it hand-composes `stranded_selfheal_action` and
then `kickoff_recovery_action` in the order its author believed the monitor used, asserts the
result, and passes — for two releases, while the shipped monitor returned before ever reaching
the second step. A test that *is* the composition cannot observe that the real composition
differs. This is the "unpinnable wiring nobody could assert a property against" hazard named
elsewhere in this file, and it is the reason a 100%-green suite reported nothing while the
feature was dead.

So the arm's precedence is lifted into `failed_arm_route(UnconfirmedDisposition) ->
FailedArmRoute`, total over the disposition enum (adding a variant without deciding its route
is a compile error, not a silent fall-through). "An eaten paste reaches the recovery; an idle
pane does not" is now a property a test reads off a pure function rather than one a reader
re-derives by tracing `return`s.

State plainly what this does **not** buy: it does not make `late_monitor` executable in a test,
and the arm could still be mis-wired to ignore the route it computes. It closes the specific
hole that actually opened — an ordering nothing could assert — and no more. The #517 test's
misleading comment is corrected in place rather than left to mislead the next reader.

**No queue mutation.** Nothing here enqueues, dequeues, or reorders; the recovery path reaches
the queue only through `enqueue_text`'s existing front door, which owns its own persistence. So
no new `persist_queues` obligation arises under the #523 table.
## The unconfirmed-delivery detector gets evidence about the AGENT (#539 / #546)

**Problem.** Every input `unconfirmed_disposition` had was about the BOX. #522 taught it to
recognise a pane that had gone idle, and #559 taught it to admit when it could not read the box
at all — but nothing anywhere in the decision knew whether the *agent* had done anything. So a
worker that had reported and pushed within the same minute could still draw the actionable "the
prompt may be sitting unsubmitted in its pane; get_output it and re-send" alarm, because
`input_pending` said human characters were outstanding. That counter is known to latch >0 over
an empty box (a bare ESC, a TUI line-clear, a CLI consuming the line — see the human-input
section above), and it had no second opinion to answer to. ~28 of these in one observed session,
each costing an orchestrator turn plus the `get_output` probe it prompts — the top remaining
performance cost of the prompting pipeline (#567's ranking C1).

#535 had already built the missing signal and named this function as its intended second
consumer: `agent_acted_since`, over the MCP activity clock stamped once at the `tools/call`
dispatch funnel. It means **the agent's own code path executed** — a live process authenticated
with that agent's token and invoked a loomux tool. It does not mean the pane painted, and
terminal output is deliberately not part of it (#480: our own paste echoes back, and statusline
and spinner frames never stop).

### Precedence: the merged table (#585 + #539)

These two changes landed against the same function within hours of each other and **both split
the `NotHolding` row**, so the composition is stated here in full rather than left to be inferred
from match order:

| `reading` | `box_pending` | `turn_evidence` | `agent_acted` | `kickoff_recoverable` | result |
| --- | --- | --- | --- | --- | --- |
| `Holds` | any | any | any | any | `Notify` |
| `Unverifiable` | any | any | any | any | `Notify` |
| `NotHolding` | `true` | `true` | `true` | `false` | `ActiveAuditOnly` (#539) |
| `NotHolding` | `true` | `true` | `true` | `true` | `Notify` (kickoff veto) |
| `NotHolding` | `true` | otherwise | | | `Notify` |
| `NotHolding` | `false` | `true` | — | — | `IdleAuditOnly` (#585) |
| `NotHolding` | `false` | `false` | — | — | `EatenNotify` (#585) |

The `—` are not shorthand for "any": `agent_acted` is **structurally absent** from the
`box_pending: false` arms, and that is the load-bearing half of the merge.

`Holds` is a direct observation that our text is sitting unsubmitted; an agent busy for reasons
that have nothing to do with our paste must not silence a strand we can see. `Unverifiable` is
#559's honest-uncertainty arm and stays **untouched** — activity is evidence about the agent, never
a reading of the box, so suppressing there would re-create the exact defect #559 fixed (silence
drawn from an answer the pane was never consulted about) merely sourced from a different signal.

**Why AND, not OR — correcting #585's own prediction.** #585 shipped anticipating that #539 would
compose as *evidence-OR*: "any independent sign that the agent acted on our delivery justifies
silence". That is not what shipped, and the difference matters in exactly the cell #585 exists to
protect. An MCP call is **not** a sign the agent acted on *our delivery* — it is a sign the agent's
process is alive. An agent whose paste was eaten still calls loomux: every role's instructions end
with "if you have no task yet, report progress and wait". Under OR, a lost kickoff on a pane with
no turn evidence would be silenced by the very report that proves the agent never got its brief —
re-deading the recovery #585 had just un-deaded, from a different input. That prediction has been
corrected in #585's own doc comment rather than left standing.

So two rules, both **restrictions** rather than extensions:

- `agent_acted` is read **only** in the `box_pending` cell. The `IdleAuditOnly`/`EatenNotify`
  split is #585's alone, and activity never reaches it.
- Inside that cell it must be accompanied by `turn_evidence`. Silence requires the pane to have
  *painted a turn since our Enter* **and** the agent to have *reached loomux after it settled* —
  two independent post-submit observations, neither of which the other can manufacture. The panes
  this was built for (agents that "had reported and pushed within the same minute") satisfy both by
  construction, so the fix loses nothing it was aimed at.

`failed_arm_route` is total over the disposition, which forced the new variant to be routed
consciously (`ActiveAuditOnly => QuietStop`) rather than falling through a wildcard into an
escalation — the property #585 added it for. One route, two reasons to be silent, two audit
actions (`delivery-unconfirmed-idle-pane` and `delivery-unconfirmed-agent-active`), so which
evidence suppressed the alarm stays greppable.

**The suppression is bounded, and the bound is the evidence itself.** `.loomux/lessons.md` requires
that any suppression driven by a fallible signal be bounded — the failure mode being a guard that
holds "while X is true" and never clears. This one has no such state: it is a one-shot decision
taken against a POSITIVE stamp that must already exist, not a hold waiting for a condition to
clear. No stamp, no suppression; a pane with no activity signal keeps its pre-#539 behaviour
verbatim. Nothing here can wedge, because there is nothing here to wait on.

**The settling floor.** `UNCONFIRMED_ACK_SETTLE_MS` is `REINJECT_ACK_SETTLE_MS`'s sibling with a
deliberately different sum. That one is measured from the moment a re-grounding was *decided*, so
it covers the whole decision→Enter chain; this one is measured from `submit_sent_ms`, stamped
immediately *before* the first Enter, so it charges only for what is still ahead of that stamp
(`SUBMIT_CONFIRM_WINDOW` + the blind-retry tail = 7.6s). What no constant here can bound is stated
rather than implied: an agent mid-turn when our Enter lands can make calls belonging to its
*previous* turn for as long as that turn runs. That residual is #546's question, and it is exactly
why the activity input governs only the arm where the box was independently observed to be clear.

**Why a recoverable kickoff vetoes the suppression.** #517's lost-kickoff re-delivery is reached
through the notify path on a `NotHolding` reading, and an agent whose brief never arrived is
precisely the agent that calls a loomux tool anyway — every role's instructions end with "if you
have no task yet, report progress and wait". On a fresh spawn's kickoff an activity stamp is as
consistent with "the brief was eaten and the agent announced itself idle" as with "the brief
landed", so it is the one case where this evidence points at nothing. A recoverable kickoff always
takes the unchanged path.

### Per-pane coalescing of the alarms that remain

The other half of the cost. Two deliveries to the same pane can each declare failure within one
`LATE_MONITOR_POLL` of each other — supersession only retires the older monitor at its *next*
tick — and an in-window `Failed` from `deliver_now` can land alongside a monitor's. Each was its
own notice asking the orchestrator the same question.

`notify_unconfirmed_delivery` now buffers into a per-`(group, agent, eaten)` bucket and the alarm
that *created* the bucket arms a single `UNCONFIRMED_NOTICE_COALESCE_WINDOW` (15s = 3 ×
`LATE_MONITOR_POLL`) timer; joiners never re-arm it, so the window cannot be extended one failing
delivery at a time (the "bounded from the ORIGINAL attempt" rule #535's busy deferral follows).
The flush emits one notice **naming every constituent**, per #533-A's collapse-at-source
convention — the verb agrees with the count, and the plural changes the ask to one `get_output`
rather than one per id.

**`eaten` is part of the bucket KEY, not a field on the batch** (#585 + #539). The two alarms
carry materially different instructions — "was LOST, its text never reached the box, re-send it"
versus "may be sitting unsubmitted, get_output it" — so one coalesced line could not truthfully
cover both, and merging them would put a claim on ids it is false for. Each kind gets its own
window and its own accurate wording; a pane raising both inside one window pays two notices instead
of one, still far below the per-delivery cost this replaced. `delivery_eaten_notice` coalesces and
names its ids exactly like the unconfirmed one, through the shared `notice_id_list` so the two caps
cannot drift.

**Delivery ids are `submit_sent_ms` stamps.** That is the identity the delivery ledger and
`late_monitor_tick`'s supersession check already key on (no two deliveries to a pane share one),
and the only id *every* delivery has — queued or straight through the front door. The same value
is now written as `delivery_id` on this pane's `delivery-failed-idle`,
`delivery-unconfirmed-idle-pane`, `delivery-unconfirmed-agent-active` and
`delivery-unconfirmed-buffered` lines, so any id in a notice resolves back to the record it came
from.

The per-delivery gates (`should_notify_unconfirmed`, i.e. confirmed / orchestrator-target) stay at
alarm time because they are per-delivery facts. The **paused** gate is re-verified at flush time
per #532 — a window is 15s of wall clock in which a human can pause the group — and its
suppression audit names every id it swallowed (#445). The id list pasted into the pane is capped at
`UNCONFIRMED_NOTICE_IDS_MAX` (the same number, for the same reason, as
`PAUSE_SUPPRESSION_LIST_MAX`: a notice is itself a paste), with the remainder pointed at the audit
log, which keeps every id. The `delivery-unconfirmed-notice` record follows the delivery attempt
and carries `delivered` + `error`, because `deliver_to_orchestrator` is best-effort and its "no
live orchestrator" branch audits nothing of its own — auditing first would assert a notice that
never went anywhere, and coalescing would make that one false record stand for the whole batch.
Nothing touches `persist_queues`: this adds no queue mutation, so #523's mutation-site table is
unchanged.

#### The window must be able to give an alarm back (rev-13 finding A)

A delay creates an ordering that did not exist before it, and this one creates a bad one.
`late_monitor_tick` checks `hook_match` *before* `already_failed`, so a late `promptsubmit` record
arriving after a failure was declared is a first-class outcome — `Confirm { correction: true }`
exists for exactly it — and the monitor keeps polling every `LATE_MONITOR_POLL`, so two or three
of those ticks land inside a 15s window. Pre-#539 the alarm was already out before any correction
could exist, so the orchestrator could only ever read "unconfirmed", then "correction, it landed".
Buffered, the correction overtakes the alarm and it reads them backwards: *it landed* at t+5s,
then *unconfirmed … id N …* at t+15s. That is a `get_output` probe plus the tempting re-send
#451/#510 exist to prevent — a **new** false alarm, manufactured by the mechanism whose whole
purpose is removing false alarms.

So `MonitorAction::Confirm` **retracts** the id from the bucket
(`retract_unconfirmed_delivery`). Retraction rather than a re-read at flush time, because the
flush has nothing to re-read *from*: `last_delivery` holds the most recent `DeliveryOutcome` per
pty, not one per delivery id, so for a multi-id batch no ledger can answer "is id *N* specifically
still unconfirmed". The path that learns a delivery resolved is the one that knows which id
resolved.

Two consequences worth stating rather than leaving to be discovered:

- **A flush removes the whole bucket**, so an id still in it has provably not been announced. That
  makes "withdrawn" and "the orchestrator never heard about it" the same fact — which is why a
  withdrawn id also suppresses the **correction notice**. A correction refers to an alarm the
  reader has already seen; for one that never went out it would be a turn spent describing a
  message that does not exist. An id whose notice *has* gone out cannot be withdrawn, so it still
  gets its correction exactly as before.
- **Emptying the bucket drops the key**, and the already-armed timer then fires into nothing
  (harmless — the flush returns early on an empty bucket). A later alarm opens a fresh bucket and
  arms its own timer, so the worst case is a notice delivered *sooner* than a full window: less
  coalescing, never a lost or a spurious notice, and never permanent silence.

#### Two residuals, dispositioned rather than left implicit

**A restart inside the window drops the buffered alarms (rev-13 N1) — accepted.**
`unconfirmed_pending` is in-memory and the timer is a detached thread, so both die with the
process. The tempting fix — a startup sweep re-noticing every `delivery-failed-idle` with no
matching notice — would be *wrong*, not merely expensive: the notice's actionable ask is
"`get_output` it and re-send if needed", and a restart destroys the pty, so there is no pane left
to read and nothing left to be stranded in. That is the same argument #522's own section already
makes for the disposition ("it holds nothing across a restart… there is no delivery left to
judge") and the same reason `StrandedSubmit` markers are deliberately never replayed. What the
lost buffer owes is a *record*, and that already exists: `delivery-unconfirmed-buffered` is
written per id at buffer time, carrying `delivery_id` and `window_ms`, so "a notice was owed and
the process died before it went out" is reconstructible from the log even though the notification
is not re-sent.

**A pause-suppressed batch is not picked up by #569's resume notice (rev-13 N2) — deliberate.**
Folding these ids into `suppressed_during_pause` is a one-line change and it would be the wrong
one twice. #569's notice is scoped to *payloads a sender was told `Ok` about and which then ceased
to exist*; an unconfirmed alarm is loomux's own observation, not lost work. And that observation
is exactly the kind a resume must not replay: it was taken when the pane was seen idle with our
text at the tail, and after an arbitrary pause that reading is stale in precisely the way #532
forbids acting on. The standing net is the right one — the pane's next delivery re-derives the box
state and flushes stranded text if any is really there — with the `notice-suppressed` audit line
naming every swallowed id meanwhile. Re-alarming at resume would need its own decision (re-observe
the pane, never replay the old reading), which is a follow-up, not a one-liner.

### #546: the claim and the evidence

The badge half of this landed first (below); the vocabulary and the durable record followed.
Read them as one decision — the finding is a single one, applied to every surface that speaks
about a finished re-grounding.

#### The rule

**A surface reporting a resolved re-grounding names what was proven, never what was hoped.**
Loomux resolves the phase on `confirmed_delivery || acked`, and those two are evidence about
different things:

| | what it is | what it proves | what it does not |
| --- | --- | --- | --- |
| `ReinjectAck::Delivered` | our submit sampler watched the Enter land | the notice reached the input box and was submitted — evidence about **our paste** | that the agent read it |
| `ReinjectAck::LivenessOnly` | the agent's own `tools/call` after the settling floor | the agent is alive and executing — evidence about **the agent** | that the paste arrived *or* that it was read |

Neither proves the read. #546 weighed the three ways it could be proven and every one is
declined by a standing constraint of this project: an acknowledgment marker makes re-grounding
a two-party protocol every agent CLI has to cooperate with, and correlating the activity stamp
against the paste content is exactly the heuristic inference #112 removed. So the honest close
is the one an audit record can actually support — say what was observed.

#### `ReinjectAck` — one place the vocabulary lives

The distinction has to be stated in four surfaces: the durable audit record, the badge's
label, that badge's tooltip, and the `AgentEntry` field the badge reads. Each previously
re-derived it from the bare `&'static str` `"delivery"`/`"activity"`, which is precisely how an
`acked`/`confirmed` claim survived in three of them at once while the string underneath was
accurate the whole time. `ReinjectAck` owns `wire()`, `audit_action()`, `proves()` and
`does_not_prove()`; every surface derives from it.

**The wire values did not change.** `"delivery"` / `"activity"` name the evidence *source*, and
that was always true. What was wrong was the **claim wrapped around them**. Keeping them keeps
#588's badge contract, an operator's saved audit query, and #535's own `source` field intact
while the claims move — the smallest change that fixes what was actually broken.

#### Two audit actions, not one action with a field

`compact-reinjection-confirmed` covered both arms. On the `LivenessOnly` arm nothing confirmed
anything, so anyone counting confirmations in `audit.jsonl` — the surface that outlives the
badge, the session and the pane — counted liveness closes among them. That is #546's finding
expressed in the most durable place it appears, and the badge fix could not reach it.

So `LivenessOnly` writes **`compact-reinjection-liveness-only`**. Two actions rather than one
plus a field, for the same reason #539 gave `delivery-unconfirmed-agent-active` its own name
instead of making it a second flavour of `delivery-unconfirmed-idle-pane`: "we watched our
Enter land" and "the agent is alive" are different *observations*, and an action name is what a
reader greps, counts and alerts on. Both payloads carry `proves` and `does_not_prove` in words,
so the record is self-describing rather than assuming the reader knows which `source` is the
weak one.

**This is a change to a queryable contract**, and it is stated as one. A consumer counting
`compact-reinjection-confirmed` will now see fewer of them — which is the fix, not a
regression: the ones that disappeared were never confirmations. Nothing in-tree consumes these
action names outside the tests (the frontend audit viewer renders actions generically), so the
break is scoped to external queries, and the two names together reconstruct the old count
exactly.

#### `Acked` → `Resolved`, on the wire as well as the label

`CompactionStatus::Acked` serialized as `{"status":"acked"}` and rendered as `re-grounding
acked (activity)`. **`acked` is the word #546 is titled after**: on that arm the agent
acknowledged nothing — it called a loomux tool for reasons of its own and loomux stopped
retrying. #588 put the source in the label but left `acked` as the head noun, which is the
overclaim with a qualifier attached rather than the overclaim removed. The variant is now
`Resolved { evidence, since_ms }` — named for what happened, the natural counterpart to
`Abandoned` — and the two labels are `re-grounding delivered` and `re-grounding unproven (agent
alive)`. Neither contains "ack", and `compactionstatus.test.ts` asserts that of both.

Both tooltips now carry a residual. The `Delivered` one is the easy one to forget: reaching the
box is not being read, and a tooltip that stopped at "the paste reached the box" invites the
reader to supply the rest.

#### What this still does not do

It does not make an activity-sourced resolution stronger, and the uncovered case #546 named
stays uncovered — a genuinely lost paste on an agent busy for another reason still ends the
retry loop, and the agent still carries on without the contract. What changed is that every
surface now says so. The delivery layer's role-agnostic Tier-1 box-structure re-submit remains
the real net underneath.

One under-report is deliberate and worth naming: a tick where `acked` fires resolves
immediately (#535's whole point — a landed re-grounding is never re-sent), so a delivery
confirmation that would have arrived seconds later is never observed and the record says
`activity`. That errs toward the weaker claim, which is the only direction an honest record can
err: the value names what loomux had in hand at the moment it stopped retrying.

#### Coverage

`every_reinject_ack_states_what_it_proves_and_what_it_does_not` pins the vocabulary itself
(both action names, both wire values, both residuals, and that the stronger evidence wins in
`from_evidence`). `a_re_grounding_closed_on_liveness_is_never_audited_as_confirmed` is the
behavioural red: it drives the real tick with **no** `DeliveryConfirmation` anywhere and asserts
zero `-confirmed` lines. Its negative pair lives inside
`compact_nudge_tick_retries_an_unconfirmed_reinjection_then_resolves_on_the_delivery` — a real
delivery confirmation keeps `-confirmed` and writes no liveness line, so "rename every
resolution" cannot pass. (Its host is
`compact_nudge_tick_retries_a_reinjection_whose_delivery_never_confirms_then_delivers_exactly_once`
— the one existing test that drives a real `DeliveryConfirmation` through to a resolution.)

**The negative control is load-bearing, and that is measured rather than assumed** (rev-157).
Building the mutation it exists to catch — `from_evidence` ignoring its argument, so every
resolution is relabeled `LivenessOnly` — leaves all three liveness-arm tests
(`a_re_grounding_closed_on_liveness_is_never_audited_as_confirmed`,
`a_landed_re_grounding_is_never_re_sent_once_the_agent_itself_answers`,
`an_in_flight_tool_call_from_the_previous_turn_is_not_an_acknowledgment`) **green** while the
whole system mislabels every resolution. What reddens is the delivery-arm pair, plus the
vocabulary test via its `from_evidence` assertion. So the honest-labeling tests cannot police a
blanket relabel on their own, and anyone later reading the delivery-arm assertions as a
redundant restatement of the liveness ones should not delete them. Hollowing `does_not_prove()`
to `""` is likewise caught twice, at two altitudes — the vocabulary *and* the audit payload the
production tick actually writes — which is why the payload assertion is not left to the pure
function alone. The two sessionstart-path assertions were extended to name *both*
actions, since asserting only `-confirmed == 0` would have started passing by omission.

### #546 option 3: the badge states which evidence closed the phase

`acked` resolves a re-grounding on one of two signals that are not equally strong. `"delivery"` is
loomux watching its own Enter land. `"activity"` is only the agent's own process reaching loomux
afterwards — it proves the agent is **alive and executing**, never that it **read** the
re-grounding. The case that stays uncovered is a genuinely lost paste on an agent that is busy for
some other reason: it resolves as confirmed, the agent carries on without the contract, and
nothing says so. It is worst on an orchestrator pane, where the notice is suppressed by design and
the badge is the only surface that reaches a human at all.

#546 weighed three options; two of them (an acknowledgment marker the agent must emit, or
correlating the stamp against the paste content) are decisions this project has repeatedly
declined — a two-party protocol every CLI must cooperate with, and exactly the kind of heuristic
inference #112 removed. Option 3 is what an honest system does when it cannot prove the thing it
wants to prove: make the residual **legible**. `AgentEntry.compact_last_ack` / `_ms` keep the
provenance the resolution's audit line already recorded, and
`compaction_status` surfaces it as `CompactionStatus::Resolved { evidence, since_ms }` for the same
recency window `Abandoned` uses. `compactionstatus.ts` renders the two evidence classes as
distinct labels — the evidence is in the *label*, not only the tooltip, the
same shape `armedQualifier`'s `(hook-confirmed)` established in #417 — and each tooltip
says outright what its signal does not prove.

(As shipped, this section's variant was `Acked { source: &'static str }` and the labels were
`re-grounding acked (delivery|activity)`. The section above replaced both: `acked` is the word
#546 is titled after, and it was still the head noun. The argument here is unchanged; only the
names it lands on are.)

Both are recent terminal outcomes, and an agent that has compacted more than once can carry a
stamp for each, so the **more recent wins** rather than a fixed ranking: a fixed one would either
let a resolved re-grounding hide a fresh loss (unsafe) or let an old loss hide today's resolution
(misleading). Ties go to the loss — the louder of the two, and the only one that asks a human for
anything.

**What this does not claim.** Naming the source does not make an activity-sourced ack stronger; it
makes it *visible*. The residual #546 opened stays open, and the notice/badge is now the place a
reader can see it rather than a line in an audit file.

### Coverage

`a_pane_whose_agent_kept_working_is_busy_not_unconfirmed` pins the live shape and its negative
(no stamp ⇒ pre-#539 behaviour verbatim, so a fix that suppressed unconditionally could not pass);
`activity_never_outranks_evidence_that_points_the_other_way` walks every arm activity must *not*
govern, including both `Unverifiable` cells; `a_recoverable_kickoff_is_never_silenced_by_the_
agents_own_idle_report` pins the #517 veto against the identical inputs that otherwise suppress,
so it cannot pass by coincidence; `activity_inside_the_settling_floor_is_the_previous_turn_
talking` pins the floor from both sides plus the never-called seed.
`several_alarms_on_one_pane_cost_the_orchestrator_one_turn_not_n` drives the two halves of the
coalescer exactly as production composes them (the only thing skipped is the sleep) and asserts
the delivered text, the audit, and flush idempotency;
`the_coalesced_notice_reads_as_one_ask_and_names_every_delivery` pins the wording both ways;
`a_group_paused_during_the_window_suppresses_the_flush_and_says_which_ids` pins the #532 re-check
and the #445 trace; `a_delivery_corrected_inside_the_window_is_never_announced_as_unconfirmed`
pins finding A end to end (a two-id bucket, one corrected mid-window, the flush naming only the
survivor in both the audit and the delivered text);
`a_bucket_emptied_by_retraction_announces_nothing_at_all` covers the empty-bucket close, the
no-op return that keeps a correction owed for an alarm already read, and that a later alarm still
opens a window (a retraction cannot wedge a pane into silence);
`the_notice_audit_records_whether_it_actually_reached_anyone` drives both directions of the
`delivered` flag against a real unbound-then-bound orchestrator. For #546, `a_resolved_re_grounding_surfaces_which_evidence_closed_it` and
`a_fresh_loss_is_never_hidden_behind_an_older_resolution` pin the derivation, the wiring is
pinned at the resolve site inside
`a_landed_re_grounding_is_never_re_sent_once_the_agent_itself_answers` (a test that stopped at the
pure function would stay green if the wiring were never written), and `compactionstatus.test.ts`
pins that the two sources never render as one label.

## Agent ids that survive a restart, and the delivery id that rides on them (#524, #455)

Two issues, one property, one change — which is the first thing to justify, since a PR that
merges two tickets owes an argument that they were one.

**The property: an agent id is minted once and never again.** Everything else here follows.
The kickoff delivery id #455 needs is unique and durable *because* the agent id is, so it
costs one format string; solve #455 alone and it needs a durable counter of its own, a second
mechanism doing the first one's job.

### #524 — the counter was in-memory, so ids recycled

`OrchRegistry::seq` is a plain `AtomicU32` and every id — `w-3`, `rev-8`, `solo-2`, `orch-1` —
takes its suffix from it. It started at 0 on every construction, so the first worker of every
launch was `w-1` whether or not a previous `w-1` still had artifacts pointing at it. The
persisted surface keyed by an agent id is wide: the board's `assignee`, audit attribution, watch
and channel identity, and the default branch/worktree name `agent/<id>`.

This is observed, not theorized. #227 is a spawn colliding with a stale `agent/rev-8` branch
after a restart reset the counter (the #227 base-descent guard caught it; the workaround was
explicit branch names). #523 had to re-key queue-orphan recovery onto `to_orchestrator` +
`session_id` because `agent_id` could point at the wrong live agent post-restart. Queue
recovery therefore stays keyed on the session and is untouched by this change; the rest of that
surface is what this fixes.

**`mint_agent_seq` is now the only way a value leaves the counter.** All four mint sites
(`spawn_agent_ex`, `solo_prepare`, `solo_adopt`, `register_orchestrator_pane`) call it, and it
does three things under one lock:

1. **Seed**, once per process, from durable state.
2. **`fetch_add`.**
3. **Persist** the new high-water mark through `atomic_write`.

All three under one lock deliberately. Persisting outside the critical section would let two
concurrent spawns write their marks out of order and leave the file *below* an id already handed
out — the exact reuse this closes — and the alternative fix, a read-modify-write of the file per
mint, is strictly more I/O for the same guarantee. Mints happen at spawn, a few per session.

**The write lands before the id is returned.** On the path where the write *succeeds*, that is
what makes the guarantee survive a crash mid-spawn: at the moment the mark is durable the caller
has not yet cut a worktree, written a roster row, or emitted an audit line, so no artifact can
outlive the mark that reserved its id. (Before this, `persist_agent_record` ran *after*
`git_worktree_add`, so even a roster-derived mark would have missed a branch left by a spawn that
died in between.)

**And the write can fail.** It is audited (`agent-seq-persist-failed`) and not propagated —
`persist_queues`' rule for `persist_queues`' reason: failing the spawn would turn a durability
degradation into an outage of the thing being made durable. But that decision is exactly what
punches a hole in the paragraph above, and the first cut of this change stated the guarantee
absolutely one paragraph before describing the path that breaks it, without connecting them
(rev-13's blocking finding on #604). Spelled out: the id is returned anyway, the caller goes on
to cut a worktree, write a roster row and emit audit lines, and the mark that reserved it never
reached disk. An artifact *does* outlive its mark. On the next restart a seed that believed the
file would reissue a live id — #524's own bug, reintroduced through #524's own degraded path,
silently.

That is not a tail risk here, which is why it decides the design rather than earning a caveat.
`atomic_write` fails on disk-full — its own `sync_all()` comment calls that "the disk-full
guard" — and this repo has three recorded disk-exhaustion incidents (#134, #320, #488), one of
which crashed loomux itself. "A failed write and then a restart" is close to the modal way this
would fire, not an exotic conjunction of independent rare events.

**So the seed heals upward, and the invariant is restored rather than caveated.** Because the
seed takes `max(file, roster)` (below), a mint whose write failed is still covered by the roster
row the spawn goes on to write: the mark reverts to being *derivable* even when it is not
*recorded*. Stated precisely, and this is the form worth keeping:

> **No artifact whose spawn reached its roster write can outlive the mark that reserved its id** —
> the file records it, and if the file's write failed the roster still shows it.

The honest scope of that sentence is the clause it now carries. A spawn that dies *between* the
failed counter write and its roster write leaves an `agent/<id>` worktree branch behind with
neither source recording the id, and that one is genuinely uncovered — it is the same
already-documented residual as a deleted orchestration root, reached a different way, and it is
narrow: two writes, both best-effort, both on the same code path, with the process having to die
between them. What is no longer true is the earlier absolute, which claimed no window at all.

### Where the mark lives — and why the roster is a second source, not a fallback

The issue offered two shapes: (a) persist the counter per group, or derive it from state loomux
already keeps. Both are in the change, and the seed takes **the higher of the two, always**.

`agent-seq.json` sits at the orchestration **root**. The counter is registry-global — one `seq`
serves every group and the solo pseudo-group — so per-group persistence would need a
max-across-every-group read before the first mint, and would still lose the mark whenever a
group dir is swept, while the artifacts keyed by the ids it handed out do not go with it. A
stale `agent/rev-8` in the human's repo colliding with a re-minted `rev-8` is #227, exactly. A
plain file at the root is invisible to both root scans (`session_roles` needs a `group.json`,
`existing_group_ids` filters on `is_dir()`). Write mechanics: see the `agent-seq.json` row in
`doc/design/durability-and-disk.md`.

**The roster is the second source.** `agents.json`, the durable roster loomux has always kept,
records every id it ever handed out; the seed scans every group's copy for the highest suffix and
takes `max(file, roster)`.

It earns its place twice over. **Migration:** nothing wrote a counter file before this change, so
on the first launch after upgrading, the roster is the only record of which ids are spent —
without it an existing install's protection would begin only *after* the collision it exists to
prevent, which is the entire population that has already hit this. **Healing:** it is what closes
the failed-write hole above, because a mint whose counter write failed still leaves a roster row.

`max` rather than a fallback is the whole point, and the first cut got this wrong — it consulted
the roster only when the file was missing or unparseable, so a file that parsed to a value *below*
the roster was believed (rev-13). The arguments against the roster are real and unchanged: as the
*mechanism* it would be worse in two directions the file is not — it misses an agent whose spawn
died before its roster write, and it forgets everything a group sweep removes. But those are
arguments against using it **alone**, which is what they were written for. A max inherits each
source's strength and neither's weakness, and no argument against the roster is an argument for
*ignoring* it when it is plainly ahead. The cost is one directory scan per process at seed, which
the migration path already paid.

**A counter at the ceiling saturates rather than wraps.** A stored `high_water` at `u32::MAX` is
the one input where the parse-side clamp does not fail in the safe direction: `fetch_add` wrapped
the atomic to 0 and reissued from `w-1`, colliding with every live id at once — and with no
`overflow-checks` in `[profile.release]` the shipped binary would do it silently where `cargo
test` panics (rev-13 N1). The mint saturates under its lock instead and audits
`agent-seq-exhausted`. Still a broken state, but broken in the direction that reuses nothing and
says so.

**Honest residual.** A git branch left in the human's repo is still not consulted. Delete the
whole orchestration root and ids restart while `agent/w-1` branches survive. That is much
narrower than what shipped before — which did it on *every* restart — but it is not nothing, and
the issue's option (b), treating `agent_id` as ephemeral-by-contract and keying every persisted
layer on the session UUID, remains the honest model. This is (a), as filed.

### A pin that asserted the bug, and why inverting it strengthened it

`a_queued_delivery_survives_a_restart_and_is_re_queued_in_order_for_a_resumed_session` (#523) used
to assert `resumed.id == _old_agent`. That was not an oversight: it pinned the collision **as
expected behavior**, with a comment explaining that ids repeat after a restart, which is why
`rebinds_to` must key on the session id — pinned so nobody would later "simplify" `rebinds_to`
back to comparing ids. It now asserts `assert_ne!`, and this is recorded here because a future
reader will otherwise find an inverted assertion with no account of why the opposite was once
deliberate.

The inversion does not weaken what the line defended; it repairs it. **In the old world that pin
could not have caught the regression it was written to guard against**: with ids colliding, an
id-keyed `rebinds_to` would have matched by coincidence and the test would still have passed. Now
the ids are guaranteed to differ, so an id-keyed rebind returns nothing and the recovery
assertion below it fails immediately. A tripwire that was decorative became one that works, and
the collision itself moved to two dedicated tests
(`agent_ids_are_never_re_minted_after_a_restart`, `a_missing_counter_file_reseeds_from_the_durable_roster`).

### #455 — a delivery id, keyed on *acted on*

No root cause is available and this does not claim one. The audit rules loomux out as the
duplicator: the kickoff text appears in exactly one `prompt` action, every `prompt-typed` row for
that agent carries `attempts: 1`, and the delivery that carried it records
`confirm_state: "confirmed"`. One paste, one Enter, one audit row — the duplication happened
after the bytes left loomux (most plausibly the CLI processing one queued paste twice, since the
pane was demonstrably mid-turn). What the issue identifies as missing is not the root cause but
**delivery identity**: nothing stamped a delivery, so no receiver could say *"I have already seen
this one"*.

So every kickoff header now carries `Delivery id: <group>/<agent>/k1`, and all four role
templates gain a `## Duplicate deliveries` section stating the rule: *a brief whose delivery id
you have already **acted on** is a duplicate — acknowledge it in one line and do nothing else.*

**Why the agent's own identity is a delivery identity.** There is exactly one kickoff per spawned
agent, and after #524 an agent id is never re-minted for the life of an install, so
`<group>/<agent>` names one delivery uniquely and forever with nothing new persisted. A resume
mints a *fresh* agent id (`spawn_agent_ex` takes a new suffix whether or not `resume_session` is
set), so a resumed session's transcript can never contain the id its new kickoff carries — the
one case where a per-agent counter alone would have failed. The `k1` tail keeps the token reading
as a delivery id rather than the agent id repeated, and leaves the namespace open.

### Composing with #517/#585, which is the half that needed care

That recovery **deliberately re-sends** a kickoff it has evidence never arrived. Two ways to get
this wrong, both avoided:

*A per-paste id* would hand the re-delivery a fresh token, so a receiver could not recognise a
re-send of work it had already done — and it would break `queue::admit`'s byte-identical
coalesce, which is the **fourth** of that recovery's own duplicate-protection layers. So the id
lives in the kickoff **text**, which `redeliver_lost_kickoff` re-admits byte-for-byte: the id
rides along unchanged, and nothing in the queue, the coalesce, or the gate stack moves.

*Keying the rule on "seen"* would make an agent no-op the re-delivery of a kickoff it never
received — turning that recovery back into the silent loss it was built to end. The rule is
therefore keyed on **acted on**: "have I already acted on this id?", never "have I seen these
bytes?". A brief you never got to act on is work that has not been done. The two cases separate
cleanly:

| what happened | id the receiver sees | what it does |
| --- | --- | --- |
| the CLI re-processed one paste | the same id, already acted on | acknowledge, no-op |
| #517/#585 re-delivered a lost kickoff | the same id, **not** acted on | act on it, once |
| a resume, or a different agent | a different id | act on it, once |

**Mid-session prompts are deliberately not stamped.** A unique id per `send_prompt` would defeat
the byte-identical coalesce that already collapses a repeated mid-session ask, and a mid-session
sender is still around to re-send — the same asymmetry `Delivery::recovers_lost_kickoff()`
already draws between a kickoff (unrecoverable if lost) and everything else.

**All four roles, not three.** An orchestrator that re-processes its own kickoff re-runs a whole
session start; a header present on three roles out of four is the asymmetry that later reads as a
bug. `orchestrator.md` additionally says how to *read* a delegate that reports a duplicate: it
did the work once, not zero times.

**What this does not claim.** It is a mitigation, not a fix: it makes a duplicated brief
*recognisable* by the receiver, which is a behavioral guardrail an agent can still fail to
follow. It does nothing about the CLI-side duplication itself, and #455 stays open for that.

**No queue mutation** anywhere in this change, so no new `persist_queues` obligation arises under
the #523 table.

## Prompt-collision mutual exclusion: compose strip + typing hold (#43)

**Problem.** Worker reports and orchestrator kickoffs are delivered by bracketed-pasting
into the orchestrator pane's PTY stdin, then pressing Enter (`deliver_prompt`). The CLI's
own input box is a *shared resource*: if the human is mid-sentence in it when a report
arrives, the paste lands inside their half-typed line and the Enter submits the merged
text. A partial guard already existed — `PtyManager::last_user_input_ms` let the *retry*
Enters skip when the human typed after the first submit — but nothing guarded the initial
paste or the first Enter, which is exactly where the corruption happens.

The fix ships two of the reviewed options together: **C** (the structural destination) with
**A** (a cheap backstop). B (focus-aware deferral) and D/E were rejected — see below.

**C — loomux-owned compose strip (structural mutual exclusion).** The orchestrator pane
gets a thin loomux input strip docked under its terminal (frontend `Pane.buildComposeStrip`,
shown only for the `orchestrator` roster role). The human types steering there; on submit,
the frontend calls `orch_steer`, which enqueues the text to the group's orchestrator through
the **same** per-pane serialized delivery path (`deliver_to_orchestrator` → `deliver_prompt`,
guarded by the per-pty `delivery` mutex) that worker reports already use. The PTY's stdin
then has **exactly one writer — loomux** — and every message (yours or a worker's) is
pasted+submitted **atomically** (whole, never interleaved). The CLI's own input box stops
being shared, so by construction your prompt can't be contaminated and can't contaminate a
report. Everything lands in the audit log (`prompt`, `from: human`).

- *Ordering — this section's original tradeoff, since superseded (#445/#470).* This bullet
  originally accepted best-effort ordering here on the grounds that threading a monotonic
  seq/queue through the shared `deliver_prompt` hot path "wasn't worth it for a low-impact reorder
  window." #445 threaded exactly that queue through anyway, for a different, higher-stakes reason
  (a hold-cap timeout was DESTROYING payloads, not just reordering them), and #470 finished the job
  by making every admission — including this strip's own steer sends — go through it: `deliver_prompt`
  now admits EVERY delivery into `pty_id`'s `VecDeque` atomically with the check for whether it's
  the first, so two sub-second sends (a steer racing a report) land, and drain, in the order they
  were admitted — not the order the per-pty `delivery` mutex happened to grant. See "Delivery queue
  (#445)"'s "Ordering" subsection for the proof. Atomicity (each message lands whole) was always the
  correctness property this strip's own C/A design depended on and remains true regardless.

- *Keyboard routing.* The strip is a plain DOM input, **not** part of xterm, so it never
  steals the terminal's keys — keystrokes only reach it while it holds focus. `Alt+P`
  (`focus-compose` in `shortcuts.ts`) or a click focuses it; **Enter** submits; **Esc** hands
  focus back to the terminal. Enter/Esc are ignored while an IME composition is active
  (`isComposing`/keyCode 229) so candidate selection doesn't submit mid-word.
- *No PTY resize.* The strip is fixed chrome built *before* `term.open`/`fit`, so the terminal
  sizes to the reduced height **once** — it is not a toggled overlay, so it never triggers the
  ConPTY resize-repaint that pollutes scrollback (the invariant the git/task/audit overlays
  also respect). The inline error-status line holds this invariant too: its row is a
  **fixed-height slot present from build time** and shown/hidden via `visibility` (not
  `display`), so a rejected-send message never changes `.orch-compose` height — and thus never
  shrinks `.pane-term` into a `resizePty` on the error path.
- *Feedback, never silent loss.* `steer_orchestrator` rejects empty text and — critically — a
  **paused** group up front (a paused group's delivery is silently suppressed, so without this
  the steered message would vanish with no trace), and a dead/absent orchestrator surfaces as
  the "no live orchestrator" delivery error. All three are shown inline under the strip; the
  typed text is restored on failure (unless the human has already started a newer draft) so a
  rejected message isn't lost. Each Enter enqueues one message and the input stays live rather
  than locking while a send is in flight (rapid sends are delivered independently — order
  best-effort per the note above).

**A — typing-aware hold (backstop for direct terminal typing).** Direct typing into the CLI
box remains possible and remains racy, so `deliver_prompt` now holds delivery **before the
paste** and **re-checks right before the first Enter** while the pane has seen a keystroke
within `USER_QUIET_HOLD` (4s), polling until human-quiet, capped at `USER_QUIET_MAX_HOLD`
(90s) so a long compose session can't starve the report queue. The held duration is audited
(`delivery-held-for-user`, with `stage` = `pre-paste`/`pre-enter` and a `capped` flag). This
composes with the pre-existing submit-retry guard, extending it back to cover the two points
that actually corrupt input.

- *Pure decision + exercised loop.* The hold/deadline choice is the pure
  `should_hold_for_user(last_input_ms, now_ms, held, quiet_window, max_hold)` (unit-tested for
  recent-typing, quiet, never-typed, the cap override, the window boundary, and clock-skew
  no-underflow). Per the #40 twice-bitten lesson (a pure fn tested in isolation isn't enough —
  the *wiring* must be exercised), the poll loop that calls it, `hold_until_quiet`, is generic
  over the keystroke source and timings and is integration-tested directly: proceeds-when-quiet,
  caps-so-reports-aren't-starved, and releases-once-the-human-goes-quiet. `wait_for_user_quiet`
  is the thin production wrapper binding it to `PtyManager::last_user_input_ms` and the shipped
  timings.

**Why not B (focus-aware deferral)?** B holds reports while the orchestrator pane is *focused*.
Once C exists, the human's keystrokes go to a loomux widget, not the CLI box, so the shared
resource is gone regardless of focus — B would only add latency (reports delayed while you
merely watch a focused pane) to solve a collision C has already made structurally impossible.
A covers the residual "typed straight into the CLI" case more precisely (on actual keystroke
recency, not focus). **D** (MCP inbox) can't wake an idle CLI turn — a typed prompt is what
does that — and **E** (stash/restore the human's partial input) has no portable primitive and
is destructive/TUI-fragile. So C+A is the whole fix; B is unnecessary for this option.

**Tests.** `steer_*` integration tests cover the guards (empty, paused-feedback, no-live-
orchestrator, unknown group), that a healthy steer reaches delivery, and that steering
resolves to the **orchestrator** (not a same-group worker), is attributed to `human`, and is
audited only under its own group (isolation). Hold-guard tests cover the loop wiring as above.
The live paste/Enter behavior against a real CLI is validated by hand (no real PTY in test
mode), consistent with the rest of `deliver_prompt`.

## Image attachments in the steering strip (#72)

The human often wants to hand the orchestrator a screenshot ("this button is misaligned",
"here's the stack trace"). A CLI can't take binary on a typed prompt, but the agent CLIs we
drive — **Claude Code** and **GitHub Copilot CLI** — both read image **files from paths** given
in the prompt text. So the strip turns a pasted/attached image into a file-on-disk plus a text
reference, and the existing steer path carries it the rest of the way unchanged.

*Copilot's equivalent (verified).* Claude Code reads an absolute image path mentioned in the
prompt via its file tools. GitHub Copilot CLI documents a native `@<path>` mention for
referencing a file in a prompt (["Using GitHub Copilot CLI"](https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/overview);
direct clipboard paste is still only a feature request — github/copilot-cli#363, #1276). Because
the documented forms differ, the reference line is **CLI-aware**: `save_attachment`'s command
returns the group's resolved orchestrator CLI (`OrchRegistry::orchestrator_cli` → `cli_for`), and
`attachmentLine(path, cli)` emits `Attached image: <path>` for `claude` and `Attached image:
@<path>` for `copilot` (unknown CLIs fall back to the plain form). The `Attached image:` label is
harmless prose to either agent; the path — bare or `@`-prefixed — is what does the work, and the
save-to-file + reference approach degrades gracefully (worst case the human sees the path text).

- *Save, don't decode.* `Ctrl+V` of a screenshot (or the paperclip → native file picker) hands
  the frontend a browser `Blob`. `pane.ts` base64-encodes the raw bytes and calls the
  `orch_save_attachment` command, which decodes and writes them **verbatim** to
  `<group state dir>/attachments/<ms>-<seq>.<ext>` via `OrchRegistry::save_attachment` —
  returning the absolute path. We never decode the image (no image crate, and deliberately no
  `getrandom`-pulling uuid crate — banned on Windows per the build notes); the `<ms>-<seq>`
  name is wall-clock ms plus a process-local `AtomicU32` so a same-millisecond multi-paste
  burst can't collide. base64 over IPC mirrors the OSC 52 clipboard bridge and survives any
  webview that won't pass raw bytes through `invoke`.
- *Reference the agent will read.* On submit, `composeSteerText(draft, paths, cli)` appends one
  per-CLI reference line (see above) per queued image after the human's typed text, and the whole
  thing goes through `orch_steer` exactly like any other steer. A message may be images-only (no
  typed text). The path form is what prompts the agent to open the file.
- *Chips with remove, before send.* Each queued image shows a thumbnail chip (a `blob:` object
  URL) with an `✕` in the strip; removing one revokes its object URL. Object URLs are also
  revoked on successful send and on pane dispose, so the webview never leaks them. The chip row
  collapses to zero height when empty (`:empty { display: none }`), so the strip keeps its
  baseline height — attaching an image is a deliberate, human-initiated growth, not the toggled
  overlay resize the strip is otherwise careful to avoid.
- *Limits + feedback.* Three limits, enforced where each actually has meaning:
    - **Per-image size** (`MAX_ATTACHMENT_BYTES`, 10 MiB) and **type** (a vetted image allowlist,
      `sanitize_attachment_ext`: png/jpg/jpeg→jpg/gif/webp/bmp) are enforced on **both** sides —
      the frontend `checkAttachment` gives an immediate toast, and the backend is the real
      backstop (rejecting oversize *before* the base64 decode balloons memory, same discipline as
      the clipboard cap, and blocking an attacker-influenced extension from steering the saved
      filename — path traversal, executable extensions).
    - **Per-message count** (`MAX_ATTACHMENTS`, 8) is a **frontend-only** compose-state cap: it
      bounds how many chips can be *queued* for one message, and the backend — which saves one
      image per call and has no notion of a "message" boundary (files accumulate across a draft
      and persist past send until the group-end sweep) — has no server-side batch to enforce it
      against. So it lives where the batch exists.
    - A **membership guard** on the backend refuses a save for any group id that isn't a known,
      created group (the dir is `root.join(group)`), pinning `group_id` to a real group token.
  The save is audited (`attachment-save`, actor `human`).
- *Cleanup policy.* Attachments are a per-group **scratch** dir with a deliberately cheap
  policy: nothing is deleted per-image (a removed chip or an abandoned draft just leaves its
  file), and the whole `attachments/` subdir is swept in `end_group` alongside the worktree
  teardown. Group state (`state.json`, audit log) lives beside it and survives. This keeps the
  hot path allocation-free and needs no reference counting; the cost is bounded by the size cap
  × a session's paste count, reclaimed the moment the group ends.

**Tests.** `save_attachment_*` integration tests cover verbatim write + path placement + audit,
the type/empty/oversize rejections (including exactly-at-cap), same-millisecond name uniqueness,
the unknown-group / traversal rejection, and that `end_group` sweeps the scratch dir while leaving
durable state. `sanitize_attachment_ext` has its own allowlist test, and `orchestrator_cli`
resolution is tested for claude/copilot/unknown groups. Frontend `steer.test.ts` covers the pure
strip logic — `checkAttachment` (type/size/count precedence), `attachmentLine` + `composeSteerText`
(per-CLI path vs `@`-mention, images-only, empty no-op, trimming), reject messages, and
`bytesToBase64` round-trips across the chunk boundary. The live paste-and-open against a real CLI
is validated by hand.

## Plan agent + mixed agent types (#47, #4)

Two related additions: a **planner** role, and **per-role** agent CLI + model.

- **Planner role.** A fourth `Role::Planner` alongside orchestrator/worker/reviewer,
  spawned through the same `spawn_agent` (`kind: "planner"`) and counting against the
  same `max_agents` delegate cap. Its template (`templates/planner.md`) scopes it to
  read-only exploration: it investigates the codebase and posts a structured plan
  (scope, files, approach, test strategy, risks/mergeability, suggested worker split) as
  a **GitHub issue comment**, `report`s a one-paragraph summary, and exits. It uses the
  shared non-orchestrator tool surface (`report` / `message_orchestrator` + read-only
  `list_agents`/`get_state`/`list_tasks`), so it cannot spawn or steer; the plan comment
  is its only intended durable output, so a planner session stays cheap and its plan
  trustworthy. The orchestrator template encodes the *when*: simple/contained work →
  straight to workers; complex/sprawling/multi-worker work, an uncertain split, or a
  human-requested plan (incl. the `agent-investigate` label) → planner first, and the plan
  feeds the worker briefs.

  **What the read-only contract enforces — structural vs instruction-backed** (the
  distinction matters; earlier drafts overclaimed it as fully structural):
  - *Structural* (mechanical, verified by tests): a planner never gets a **worktree** —
    the spawn cwd logic runs it in `group.repo` even when `worktree: true` is passed; and
    its CLI is launched **read-only** (`build_agent_command(Containment::ReadOnly)`): on Claude
    `--disallowedTools Edit Write NotebookEdit` plus `Bash(git commit *)` /
    `Bash(git push *)` (`CLAUDE_EDIT_DENY_TOOLS` + `CLAUDE_READONLY_DENY_GIT`), on Copilot
    `--deny-tool write` plus `shell(git commit|push)` (`COPILOT_EDIT_DENY_TOOLS` +
    `COPILOT_READONLY_DENY_GIT`) — deny rules
    override the allow list / permission mode on both CLIs (Claude: `dontAsk`, #465
    below; Copilot: `--allow-all-tools`). So a planner **cannot edit files,
    commit, or push**, i.e. cannot produce code changes or push a branch.
    (Rule-spelling note: on Claude the `:*` wildcard is valid *only* as a trailing suffix.
    An earlier draft also passed the colon-mid forms `Bash(git commit:*)` / `Bash(git push:*)`
    as redundant spellings; those are **malformed** — Claude Code ignores them *and* prints a
    startup warning, which was the "auto deny rule" flash seen on planner boot. The canonical
    space form is the only spelling now emitted; see the plan-mode decision below.)

    **#448: this list is string literals against an external, drifting registry — now
    pinned, not just asserted.** Both CLI vendors' tool/permission surfaces are outside
    loomux's control, and until #448 nothing checked that a denied name still matched a
    real tool: `MultiEdit` (Claude folded it into `Edit` upstream) sat in the Claude list
    matching nothing, its only symptom a startup warning nobody was reading
    (`Permission deny rule "MultiEdit" matches no known tool`) — and `edit` sat in the
    Copilot list even though the CLI's own configuration guide documents exactly three
    `--deny-tool` value shapes (`shell(COMMAND)` / bare `write` / `MCP_SERVER(tool)`),
    with no `edit` among them and no startup warning to catch the mistake at all. Both
    were dropped; the guarantee on each CLI now rests only on names confirmed against
    that CLI's official reference (Claude's
    [Tools reference](https://code.claude.com/docs/en/tools-reference), Copilot's
    [CLI configuration guide](https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/configure-copilot-cli),
    both fetched as raw text and cited in #448's PR, never inferred). `Edit`/`Write`/
    `NotebookEdit` on Claude and bare `write` on Copilot are each independently
    confirmed to cover their CLI's whole file-modification surface, so removing the
    unconfirmed names does not narrow the guarantee — it only removes the false
    confidence of a denial that looked like containment but wasn't.
    `claude_edit_deny_tools_are_known_claude_tools` and
    `copilot_edit_deny_tools_are_known_copilot_categories` (`tests/orchestration.rs`)
    pin each CLI's list against that CLI's documented set, so a future typo or a stale
    name reintroduced into either list breaks CI instead of silently reproducing this
    bug. **What the pin does not do:** it cannot detect a *future* upstream rename or
    removal against an unrefreshed snapshot — only a human re-running the refresh
    procedure documented on `KNOWN_CLAUDE_TOOLS` can catch that half; there is no
    machine-readable tool registry either CLI exposes at runtime for loomux to query
    instead (`cliprobe`'s `--help` probe lists flags, not tool names, and actually
    launching a CLI to observe its own validation would count as the "spawn a real
    agent CLI to test/validate" this repo does not do, CLAUDE.md constraint 3).
  - *Instruction-backed* (the template + kickoff `PLANNER_READONLY_NOTE`, not a sandbox):
    `gh` stays allowed (a planner needs `gh issue comment` for its deliverable), so a
    planner *could* technically run `gh pr create` or create an inert local branch — it is
    told not to, and with commit/push denied such a branch carries nothing. This is a
    deliberate trade (plan-comment-as-deliverable over a full jail), now stated honestly
    rather than presented as an absolute guarantee.

  **#465: the deny list is ALSO fail-open in the other direction — closed for Claude,
  documented open for Copilot.** #448 hardened the *dead-entry* direction (a deny name
  that stops matching anything). The opposite direction stayed open: a deny list can
  never cover an editing tool that did not exist when the list was written. If a CLI
  ships a new file-editing tool tomorrow, `CLAUDE_EDIT_DENY_TOOLS`/
  `COPILOT_EDIT_DENY_TOOLS` don't mismatch, nothing warns, and the tool just works —
  *permitted by omission*. The only fix that actually closes this (rather than adding
  one more name to chase) is inverting to an allow-list: deny everything by default,
  name what's permitted. Whether each CLI actually offers that mechanism, and whether
  loomux can safely use it, had to be argued per CLI, not assumed:

  - **Claude: closed, via `--permission-mode dontAsk`.** Per the official
    [permission modes reference](https://code.claude.com/docs/en/permission-modes.md)
    ("Allow only pre-approved tools with dontAsk mode", fetched as raw markdown,
    verified 2026-07-29): *"If you set `dontAsk` mode, Claude Code auto-denies every
    tool call that would otherwise prompt you. Claude runs only actions matching your
    `permissions.allow` rules, read-only Bash commands, and calls approved by a
    PreToolUse hook."* A read-only agent (`build_agent_command`'s `read_only`) now runs
    `dontAsk` instead of `auto` (`claude_effective_permission_mode`, #465) — see that
    function's doc for the full argument. This is a genuine allow-list: a brand-new
    Claude Code editing tool released tomorrow is not in `--allowedTools`, so `dontAsk`
    denies it with zero loomux code change, closing the direction #448's per-name lists
    structurally could not. `--disallowedTools` (`CLAUDE_EDIT_DENY_TOOLS` + `CLAUDE_READONLY_DENY_GIT`)
    stays emitted alongside it, unchanged — the two layers catch different failure
    modes (named-and-stale vs. unnamed-and-new) and dropping either narrows the
    guarantee. The property this actually depends on —
    that a read-only agent's `--allowedTools` never contains a BARE tool grant that
    would silently re-open the same hole from the allow side — is pinned by
    `claude_readonly_allowed_tools_contain_no_unscoped_grant`
    (`tests/orchestration.rs`), not just asserted.

    *A side effect worth stating plainly, and correctly (review round 1, #489):*
    `auto` mode's background safety classifier used to let a planner run an ad hoc shell
    command (e.g. a quick `cargo check`) with no prior approval; `dontAsk` has no
    classifier fallback, so only `git`/`gh` (already pre-approved via
    `CLAUDE_UNATTENDED_ALLOW`) and Claude's built-in read-only Bash set (`ls`, `cat`,
    `grep`, `find`, read-only `git`, …) are reachable by default now.
    `templates/planner.md`'s protocol says so (re-blessed in `tests/fixtures/pre222/`).

    **There is currently no per-repo opt-in beyond that — for any repo, by any
    mechanism.** An earlier draft of this note claimed a persona `allow:` pattern
    could widen it (`PersonaInject::extra_allow`, "already ordered before
    `--disallowedTools`"); that was checked against the code and is false. Two
    independent, deliberate guards refuse it, both from #222's capability closure:
    `workflow::parse_workflow` hard-errors on `allow:` for any read-only block before
    a group can even launch (`workflow.rs:891`); and — belt-and-braces, for a pattern
    that reaches loomux any other way (a `.github/agents/*.md` persona's own `allow:`
    frontmatter, a hand-edited `group.json`) — `persona_inject` unconditionally empties
    `extra_allow` for a read-only block regardless of source (`mod.rs:18383-18392`,
    audited via `audit_allow_denied`, never silent). `PersonaInject::extra_allow` really
    is ordered before `--disallowedTools` in the command builder — but for a planner it
    is always the empty list by the time it gets there, so the loop never has anything
    of the planner's to iterate. The capability regression under `dontAsk` is therefore
    **absolute, not opt-out**: under `auto` the classifier was an (unreliable, unaudited)
    fallback; under `dontAsk` there is no fallback and no configuration escapes it.

    That absoluteness is a deliberate #222 stance ("nobody can enumerate every
    write-capable program" — `workflow.rs:876-890`), not an oversight this PR
    introduced, and re-opening it is a capability decision — which mechanism could
    pre-approve *some* shell commands for a read-only role without also handing it an
    unenumerable set of write-capable ones — not an enforcement-robustness fix, so it
    is filed separately rather than built here: **#490**, which names both guards
    concretely. The one escape hatch that already exists and is NOT gated by either
    #222 guard: the target repo's own `.claude/settings.json` `permissions.allow`
    rules, which `dontAsk` honours directly per the quoted doc above — a different
    trust surface (repo-owned Claude config, not workflow-controlled), worth naming
    but not a loomux mechanism.

    **So: can a planner still ground a plan with no opt-in at all, permanently?**
    Mostly, with named gaps. What still works: exploration (`Read`/`Grep`/`Glob`, which
    the permission system exempts from approval entirely — see the "Read-only" row this
    doc's table above cites), the built-in read-only Bash set, `git`/`gh` (status, diff,
    log, `gh issue view`, the `gh issue comment` plan output), and `mcp__loomux`
    (`report`, `message_orchestrator`). What's lost, concretely: anything that requires
    *executing* code to answer a question — confirming a compile error is real (not just
    plausible from reading), running an existing test to see current behavior before
    proposing a change to it, checking whether a dependency actually resolves, or timing/
    profiling anything. A plan that would have said "confirmed: `cargo check` reproduces
    the reported error at line N" now says "reading the code, this looks like it would
    reproduce the reported error" — a real loss of grounding, silent in the sense that a
    vaguer plan doesn't announce *why* it's vaguer. `templates/planner.md`'s "say so in
    the plan rather than assuming it ran" instruction is the mitigation available today:
    it turns a silent gap into a stated one, not a closed one.

    *(Doc-fetching was on this lost list until #610 and no longer is — that section
    below records the decision and its residual. The execution gap above is unchanged.)*

  **#610: an allow list `dontAsk` never received — and the clause the argument above was
  missing.** Everything above is the right argument and it shipped in a form that could
  not take effect. Three planner panes in one session had `gh` **entirely** denied (plus
  `git fetch` and `WebFetch`) while read-only `git` kept working, so a planner could not
  read its own brief or post its plan; the orchestrator relayed both by hand. Two
  hypotheses were filed with the issue and both were wrong; the docs and the session's own
  evidence settle it three ways:

  - *Refuted — "`dontAsk` does not honor `--allowedTools`."* `mcp__loomux` is a
    `--allowedTools` value and it demonstrably worked on every affected pane (the relay
    workaround ran through `report`/`message_orchestrator`).
  - *Refuted — "the CLI takes one value per flag occurrence."* Per the
    [CLI reference](https://code.claude.com/docs/en/cli-reference), `--allowedTools` takes
    space-separated values in one occurrence; the page's own example is
    `"Bash(git log *)" "Bash(git diff *)" "Read"`.
  - *The actual defect — flag ordering.* #417 inserted `--settings` **between**
    `--allowedTools`'s first value (`mcp__loomux`) and the `"Bash(git *)" "Bash(gh *)"`
    patterns that follow it. A space-separated value list ends at the next flag, so from
    that release on the patterns were not allow rules at all — they were stray positional
    arguments. `mcp__loomux`, the single value emitted before `--settings`, is exactly the
    single capability the planners kept. `PersonaInject::extra_allow` (a worker's or
    reviewer's, never a planner's — see the capability closure above) rode the same
    truncation.

  It stayed invisible for two releases because only a `dontAsk` pane can observe it:
  workers and reviewers run `auto`/`acceptEdits`, which approve `gh` without consulting an
  allow list at all. That is the general lesson worth keeping — **the pane with the
  tightest permission mode is the only honest test of a permission change**, and every
  other tier is masking, not corroboration.

  The fix is two layers, one repairing the defect and one moving the guarantee to the
  surface the contract actually names:

  1. *Ordering.* The whole `--allowedTools` value list is emitted contiguously, with
     `--settings` after it, in both `build_agent_command` and `build_agent_argv`.
     `claude_allow_patterns_are_not_severed_from_the_allowedtools_flag` pins contiguity
     rather than presence, across every tier and both spawn forms, so the next flag added
     to that branch cannot silently reopen this. Its twin
     `claude_deny_patterns_are_not_severed_from_the_disallowedtools_flag` (review N1) does
     the same for `--disallowedTools`, which pins no live defect but closes the same
     coverage gap on the side that fails **open**: every pre-existing pin on deny ordering
     was built with `hook_settings: None`, and `--settings` is a flag that appears only on
     a real spawn — a severed *allow* announces itself immediately, a severed *deny* is
     silent.
  2. *Allows must live in settings.* This is the clause the #465 argument above was
     missing, and it is worth stating as a rule rather than a fix: **the quoted contract
     for `dontAsk` names `permissions.allow`, a settings-file concept, and nothing in
     Claude Code's docs says `--allowedTools` values become `permissions.allow` rules.**
     The mode's own doc sentence was being satisfied by a *different* mechanism than the
     one it names — which is fine right up until a parse detail changes, as one had. So a
     read-only pane's rules now also go where the contract points them:
     `CLAUDE_READONLY_SETTINGS_ALLOW`, written into the `--settings` file loomux already
     generates (#417). `--allowedTools` stays too — the two layers are cheap and
     independent, the same belt-and-braces stance #465 takes on `--disallowedTools`.

  Consequences of (2) worth naming:

  - **The settings file is no longer hooks-only, and no longer optional for a read-only
    pane.** It used to be written only when there were hooks to put in it (`None`
    otherwise, and the caller omitted `--settings`). Fail-open is right for a hook — the
    cost is a missed compact nudge — and wrong for permissions, since a `dontAsk` pane
    with no allow rules can do nothing at all. A read-only pane therefore always gets a
    file now; the empty case (`None`) survives only for a non-read-only agent with no
    hooks. Keys that would be empty are omitted rather than written as `{}`: `--settings`
    values "override the same keys" per the CLI reference, so an empty `hooks` object is a
    claim, not a placeholder.
  - **Additive, on a cited guarantee.** Per the
    [settings reference](https://code.claude.com/docs/en/settings), permission rules
    "merge across scopes rather than override", so loomux's layer never displaces the
    user's own allow/deny rules and a user `deny` still beats every allow loomux writes.
    That sentence is the one doc-grounded assumption this layer rests on; loomux never
    parses or rewrites the user's settings to achieve it.
  - **`git fetch` needed no decision, only the fix.** It is a git subcommand, so
    `Bash(git *)` covers it, and `CLAUDE_READONLY_DENY_GIT` carves out only
    `commit`/`push`. It writes nothing but local remote-tracking refs and mutates no
    remote, which is why it is not denied — it was unreachable purely because the whole
    allow list never arrived.
  - **`WebFetch`/`WebSearch`: allowed, deliberately.** The paragraph above used to record
    "pulling a reference down to ground a plan" as permanently lost; that loss was never
    load-bearing enough to keep, and this repo's own `agent-cli-reference` skill *requires*
    reading a vendor's official reference before designing anything CLI-dependent — a
    planner that cannot fetch is a planner that must design from recall, which is the
    failure mode that skill exists to end. Both tools are non-mutating, and the read-only
    tier's guarantee is about mutation, so nothing about the containment contract moves.
    They are **bare grants and are recorded as such**: the permissions reference states
    `WebFetch(domain:*)` "is equivalent to a bare `WebFetch` rule", so a scoped-looking
    spelling would be decoration, and loomux (constraint #8) cannot know which domains a
    given repo's planner needs. The residual is real and stated: an arbitrary-host fetch is
    an injection surface and a data-egress channel. The escape hatch needs no loomux
    feature — a `WebFetch` deny rule in the target repo's own `.claude/settings.json` beats
    any allow rule here, in every mode.
  - **The denies had to move with the allow (review B1).** `Bash(git *)` is a *prefix*
    pattern, so as a live `permissions.allow` rule it positively matches `git commit` and
    `git push` — and before the review, the only thing carving them back out was
    `--disallowedTools`, i.e. an *argv* deny expected to beat a *settings-file* allow. That
    is precisely the cross-mechanism equivalence the clause above refuses to assume for
    allows, so assuming it for denies would have been the same mistake pointed the other
    way — and pointed at the read-only tier's one real guarantee. Worth stating plainly
    because it is the failure shape of #610 itself: a permission mechanism that *looks*
    right on argv, unobserved because nothing tests it.

    Two things closed it, and both were worth doing. First, it is in fact **documented**,
    not assumed — the [permissions reference](https://code.claude.com/docs/en/permissions)
    puts the flags in the same precedence domain as settings rules ("Permission rules
    follow the same settings precedence as all other Claude Code settings", with **"Command
    line arguments: temporary session overrides"** as level 2, and naming these very flags:
    *"If a tool is denied at any level, no other level can allow it. For example, a managed
    settings deny can't be overridden by `--allowedTools`, and `--disallowedTools` can add
    restrictions beyond what managed settings define."*), and settles deny-vs-allow within
    a level outright (*"Rules are evaluated in order: deny, then ask, then allow. The first
    match in that order determines the outcome, and rule specificity doesn't change the
    order"*; *"deny rules from any scope are evaluated before allow rules"*). Second, the
    denials are now **also** written into the same `permissions` object as the allow
    (`readonly_settings_deny`, derived from the same two `Containment` predicates
    `--disallowedTools` uses, never a third literal list) — so the rule and the rule that
    beats it live in one object and the cross-layer question does not have to be asked at
    all. `--disallowedTools` stays emitted: the same belt-and-braces stance this section
    takes for allows, for the same reason.

    *Two layers means a drift question, and it is answered by equality, not by presence
    (review N4).* Deriving both from one pair of predicates makes divergence a deliberate
    act rather than an accident, but "both lists contain today's constants" is a weaker
    claim than the one the design makes. A future third deny constant added to the argv
    branch alone would be forced onto the argv side by the exact-equality full-line
    goldens and onto the settings side by nothing — leaving a denial in one layer only
    while `Bash(git *)` stays live in `permissions.allow`, which is the B1 finding above
    one constant later. So `readonly_pane_settings_carry_permissions_allow` takes the
    planner's real `--disallowedTools` values off its spawn request and `assert_eq!`s them
    (sorted) against the settings `permissions.deny` array: the two layers are pinned as
    **the same list**, not as two lists that happen to agree today.
  - **#465's no-bare-grant invariant is restated for the new layer, not weakened.**
    `readonly_pane_settings_carry_permissions_allow` asserts that every rule in the
    settings allow list is `mcp__loomux`, a scoped `Name(...)` pattern, or one of the two
    enumerated research tools — so a bare `Bash`, or anything in `CLAUDE_EDIT_DENY_TOOLS`,
    fails CI on the allow side exactly as
    `claude_readonly_allowed_tools_contain_no_unscoped_grant` already makes it fail on the
    flag side. The exemption is a closed, named set of two non-mutating tools, not a
    loophole.

  - **Copilot: documented open, not closed — and the issue's "no containment at all"
    premise needs a correction first.** `COPILOT_EDIT_DENY_TOOLS` still denies
    `write` today (#463 dropped only `edit`, the unconfirmed entry) — so the claim that
    a Copilot planner currently has *zero* CLI-level file-edit containment overstates
    the code as it stands. It understates something else, though: per Copilot's
    [CLI command reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference)
    ("Tool permission patterns", raw markdown via `raw.githubusercontent.com`, verified
    2026-07-29), `write` is documented as a stable **category** — *"`write` | File
    creation or modification | `write`, `write(src/*.ts)`"* — covering Copilot's whole
    file-mutation surface (`create`, `edit`, `apply_patch`, per the same page's "Tool
    availability values" table), not a per-tool-name literal the way Claude's
    `--disallowedTools` list is. A brand-new Copilot editing tool is likely to land
    under this SAME existing `write` Kind (that's what a stable category is for), which
    makes today's deny meaningfully more new-tool-resistant than #448's Claude-side
    literal enumeration ever was — though "likely" is doing real work in that sentence;
    it is not a proof, and the category could still gain a sibling Kind for some future
    tool shape (a `patch` Kind alongside `write`, say) that a bare `write` deny would
    not reach.

    The real allow-list mechanism, per the same reference: *"The `--available-tools` and
    `--excluded-tools` options support these values"* — an enumerated built-in tool
    catalog (shell/file-op/agent/other) — and, per the companion
    [allowing-tools guide](https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/allowing-tools)
    ("Layers of tool controls"): *"`--available-tools` disables all tools other than
    those you specify... If a tool is not in the available set, the AI model won't be
    able to use it at all, even if you specify it with the `--allow-tool` option."* This
    is candidate (1) from the issue, confirmed to exist. **It is not wired in, and the
    reason is a specific, load-bearing unknown: neither page ever states whether
    `--available-tools` also gates MCP-registered tools**, or whether it scopes only the
    enumerated built-in catalog. loomux's planner depends entirely on an MCP tool for
    its one required output — `report`/`message_orchestrator`/`note_directive` reach
    Copilot through the `loomux` MCP server, addressed via the SEPARATE `--allow-tool
    loomux` (already unconditional in `build_agent_command`'s copilot branch) — governed
    by the allow/deny "Kind" vocabulary, a vocabulary the "Tool availability values"
    table never mentions at all. That silence is suggestive (the two vocabularies read
    as independent control surfaces) but it is an inference from absence, not a quote,
    and the brief for this issue is explicit that an inference is not a finding here.
    Getting this wrong is not symmetric: worst case, `--available-tools` also strips
    `loomux`'s MCP tools, and a planner silently can never call `report` — a hang the
    orchestrator only notices on a timeout, with no diagnostic pointing at the cause.
    That is a worse failure than the fail-open gap it would close, and confirming it
    needs an actual `copilot` run, which CLAUDE.md constraint 3 reserves for a human, not
    an agent (the same reasoning #463 already used for "does an unmatched `--deny-tool`
    value warn, error, or silently no-op on Copilot" — left as a documented unknown
    rather than guessed).

    **So: open, loud, and actionable, not silent.** `--available-tools` is the
    identified fix; the blocker is named; the next step is a one-time human-supervised
    check (start a `copilot` session with `--available-tools` set to the read-only tool
    catalog minus `create`/`edit`/`apply_patch`, confirm `report` still reaches the
    orchestrator) before wiring it in. Recommend labelling that check, and #462
    (reviewer containment, closely related — the issue that raised #465 suggested doing
    both together), `agent-ready` once someone can run it.

  **The honest table, per role and per CLI** (mirrors the prose above; `git commit`/
  `git push` denial tracks the file-edit column on every read-only row — a
  reviewer row is contained but NOT read-only, and keeps its shell git, #462):

  | Role | CLI | File-edit containment | Mechanism |
  | --- | --- | --- | --- |
  | Planner | Claude | **Structural**, both directions | `dontAsk` (#465, new-tool direction) + `--disallowedTools` (#448, named-and-stale direction) |
  | Planner | Copilot | **Structural**, dead-entry direction only | `--deny-tool write` (category-level; #465 argues this is more new-tool-resistant than a literal list, not proof against one) |
  | Reviewer | Claude | **Structural**, named-and-stale direction only | `--disallowedTools Edit Write NotebookEdit` (#462, same `CLAUDE_EDIT_DENY_TOOLS` as the planner). **Not** `dontAsk`: it would auto-deny the shell the job runs through, so the new-tool direction stays open here — see the reviewer-containment section below |
  | Reviewer | Copilot | **Structural**, category-level | `--deny-tool write` (#462, the same category grant the planner gets); Copilot has no `dontAsk` analogue to close the other direction either |
  | Worker | either | N/A by design | Workers exist to edit/commit/push; no containment is the correct posture |
  | Planner | Gemini | **Structural**, named-and-stale direction only | admin-tier policy `deny` rules on `write_file`/`replace` **plus** `run_shell_command` denials narrowed by `commandPrefix` to `git commit`/`git push`, mirrored in `tools.exclude` (#267) |
  | Reviewer | Gemini | **Structural**, named-and-stale direction only | admin-tier policy `deny` rules on `write_file`/`replace` (`GEMINI_EDIT_DENY_TOOLS`), mirrored in `tools.exclude`; `run_shell_command` untouched, same trade as the Claude row (#267) |
  | Reviewer/Planner | Codex | **Not possible** | recorded, not shipped — codex has no tool-level edit deny, and neither sandbox rung fits (see *Which CLIs may host a contained class*, below) |

  #### Reviewer containment: what is structural and what is not (#462)

  #448 found that a reviewer was covered by **none** of the above: `Role::is_read_only()`
  matched `Role::Planner` alone and fed `build_agent_command`'s `read_only` parameter, so a
  reviewer launched with no `--disallowedTools`/`--deny-tool` flags at all. It was tracked
  separately because it is a *capability* decision, not an enforcement-robustness fix.
  #462 is that decision, and it went **half** the distance on purpose.

  The deny tier is no longer a bool. `Role::containment()` maps the closed capability enum
  onto a three-rung ladder (`Containment`), and `build_agent_command`/`build_agent_argv`
  take that value instead of `read_only`:

  | tier | classes | editing tools | `git commit`/`push` | unattended regardless of `auto_ops` |
  | --- | --- | --- | --- | --- |
  | `None` | orchestrator, worker | — | — | — |
  | `NoEdits` | **reviewer** | denied | — | — |
  | `ReadOnly` | planner | denied | denied | yes |

  A `match` on the closed enum rather than a per-call bool is the point: a fifth capability
  class cannot be added without deciding, at compile time, what it may do, and
  `every_capability_class_pins_its_deny_tier` (`tests/orchestration.rs`) pins the mapping —
  including that a reviewer is contained but is **not** `is_read_only()`, which still gates
  the `allow:`-ban on a fully read-only block and deliberately did not follow the deny flags
  onto reviewers (a reviewer keeps its shell anyway, so `allow:` widens nothing for it).

  **Structural for a reviewer:** the file-editing tools — Claude `--disallowedTools Edit
  Write NotebookEdit`, Copilot `--deny-tool "write"`, the SAME `*_EDIT_DENY_TOOLS` constants
  the planner uses. One list, not a `REVIEWER_*` near-duplicate: "which tools edit files" is
  a fact about each CLI's registry, not about a role, and a second copy would be a second
  thing to keep pinned against `KNOWN_CLAUDE_TOOLS` / `KNOWN_COPILOT_DENY_CATEGORIES` — with
  a silent failure mode, since a stale deny entry reads exactly like containment (the #448
  bug). Where the classes genuinely differ, the constants differ too: the git denials stay
  `ReadOnly`-only.

  **Instruction-tier for a reviewer, and deliberately so:**
  - *`git commit` / `git push`.* A reviewer's job runs through the shell: it runs the tests,
    and `gh pr checkout <n> --detach` is how it gets the code to run them against. Its own
    template also hands it `git commit` explicitly, as the sanctioned alternative to the
    forbidden `git stash` (#299 — one stash stack shared across every worktree). "Never
    pushes" therefore stays what it has always been: a rule the reviewer is told, bounded by
    the fact that it works in its own throwaway worktree (#359) and that no downstream gate
    reads a reviewer's branch.
  - *Writing files at all.* This is the honest boundary, and it must not be implied away:
    denying `Edit`/`Write` removes the **frictionless, default** path to editing a file — the
    one an agent takes without deciding to — and leaves the shell path (`sed -i`, a heredoc,
    `python -c`) wide open, because closing it would mean denying `Bash`, and `Bash` is the
    job. So this is containment of the *accident*, not of the adversary. It is worth having
    at that size (a reviewer nudged by a diff under review toward "just fix it" now hits a
    wall instead of a temptation) and worth stating at that size, because a reviewer's write
    surface is *not* closed and a reader of this doc must not come away thinking it is.
  - *The new-tool direction #465 closed for the planner.* Its argument transfers word for
    word — `CLAUDE_EDIT_DENY_TOOLS` cannot name an editing tool Claude Code ships tomorrow,
    so a reviewer would get it, permitted by omission. Its **remedy does not transfer**:
    `--permission-mode dontAsk` auto-denies every call outside `--allowedTools`, which for a
    reviewer is the shell it runs the tests through — #465's own doc names the reviewer as
    the case it must never be used for. Closing this would need a mechanism that separates
    "a new editing tool" from "a shell command", and neither CLI offers one today. So the
    reviewer row in the table above is `named-and-stale direction only`, deliberately, and
    the gap is recorded here rather than left for someone to discover by reading the
    planner's row and assuming symmetry.
  - *Not a promotion.* `NoEdits` deliberately does not set `forces_unattended`: a reviewer in
    a non-`auto_ops` group still runs under `acceptEdits` with no pre-approved git/gh, exactly
    as before #462. Extending deny flags to a class must only ever narrow it — the snapshot
    rows in `build_agent_command_full_line_snapshots` pin every non-deny byte identical to the
    worker row on the same `auto_ops` setting.

  The reviewer template says all of this to the reviewer too, so its first denial reads as
  policy rather than as a broken environment — including the one write a review legitimately
  needs (a body too long for `gh pr review --body`) and its shell route.

  #### Which CLIs may host a contained class (#267)

  #462 made containment a property of the capability *class*. #267 stage 2 — a
  reviewer on a different model family than the worker — made the other half
  explicit: containment is also a property of the **CLI**, because every rung of
  that ladder is enforced by the vendor's own permission engine, and vendors do
  not agree on what one can express.

  That produced `CliCaps` / `CLI_CAPS` (`orchestration/mod.rs`): one row per agent
  CLI loomux has evaluated, recording three things that had been conflated into
  "is it in `SUPPORTED_CLIS`".

  | field | question it answers |
  | --- | --- |
  | `orchestration` | does loomux have a group-spawn adapter for it? (⇔ `SUPPORTED_CLIS`, pinned by `supported_clis_match_the_capability_table`) |
  | `mcp_argv_seam` | can its per-agent MCP config be delivered *entirely on argv*? |
  | `max_containment` | the deepest `Containment` tier loomux can actually enforce on it |

  `cli_can_host(cli, role)` is the gate: a block whose class needs deeper
  containment than its CLI can enforce is refused, at `parse_workflow` (so a repo
  learns from its own file) **and** at `spawn_agent_ex` (so a hand-edited
  `group.json` cannot route around the parser). The check is one comparison
  against the `Containment` ladder, deliberately — a future CLI is admitted by
  writing its row, not by editing the gate.

  **`mcp_argv_seam` is not a synonym for `orchestration`, and that distinction
  removed a latent panic.** `solo_prepare` derived its seam from `SUPPORTED_CLIS`
  and then matched the per-CLI MCP *flag string* with an `unreachable!()` arm, so
  the first CLI whose config is not argv-deliverable would have crashed a solo
  launch. PR #323 named the gap when Ante hit it; gemini is the CLI that made it
  real, and the table is that decoupling. A solo gemini pane is delivery-only for
  exactly this reason — nothing loomux can append to a command line the human owns
  carries an MCP server for it.

  ##### Gemini: files, not flags

  Gemini's two loomux-critical seams are generated files, which is why its
  `build_agent_command` arm is the shortest of the three:

  - **MCP.** Servers are a `settings.json` key (`mcpServers`, `httpUrl` +
    `headers`) with no `--mcp-config` equivalent. loomux generates the per-agent
    settings file next to every other CLI's config and hands it over with
    `GEMINI_CLI_SYSTEM_SETTINGS_PATH` — the *system*-settings override, the top
    settings tier, documented for precisely this shape in gemini's own enterprise
    guide. **Not `GEMINI_CLI_HOME`**: that relocates the whole user-level config
    *and state* root, including the credentials the human logged in with, so a
    per-agent home would meet every agent with a login prompt. On argv the agent
    then gets `--allowed-mcp-server-names loomux`, gemini's analogue of claude's
    `--strict-mcp-config`.
  - **Containment.** The policy engine is the only gemini surface that can deny a
    built-in tool *by name* (`--allowed-tools` is documented deprecated in favour
    of it), so a contained agent gets an admin-tier policy TOML named by
    `adminPolicyPaths` in that same settings file, **and** the same denials again
    as `tools.exclude`.

  **Two deny layers is not belt-and-braces for its own sake** — each covers the
  other's *documented* failure mode. Supplemental admin policies (the
  `adminPolicyPaths` kind) are ignored outright if any `.toml` exists in the
  machine's standard system policy directory, so on a box with enterprise gemini
  policies installed the TOML silently does nothing; `tools.exclude` is not
  subject to that guard. In the other direction `tools.exclude` is documented as
  deprecated in favour of policy rules, so it is the layer with an expiry date.
  Either alone is a containment that fails silently and **open**, which is the one
  failure mode #448 exists to eliminate.

  Both layers outrank `--approval-mode yolo`, which matters because an `auto_ops`
  gemini agent runs in it: gemini's own policy config gives the yolo allow-all
  rule priority `998` in the **default** tier (`1.998` after the tier transform),
  against `4.4` for a `tools.exclude` deny and `5.x` for an admin-tier rule.

  What gemini does **not** get, stated rather than left to be discovered: no
  native custom-agent flag (its persona rides the kickoff prompt, like an
  inline-`prompt:` copilot block), no `allow:` widening (those patterns are
  Claude/Copilot tool-matcher strings — translating them would be inventing
  semantics), no compact nudge (its command is `/compress`, not `/compact`;
  teaching #287's machinery a per-CLI spelling is a separate piece of work), and
  no `session_digest` (#324/#646's transcript reader has a claude arm and a
  copilot arm; gemini falls to its `other =>` error arm — which is moot today
  anyway, since gemini mints its own session ids and loomux therefore records
  `session_id: None` for a gemini agent, so no digest ever reaches that call).

  ##### Codex: evaluated, recorded, not shipped

  Codex has a row in `CLI_CAPS` with `orchestration: false` and
  `max_containment: None`, because an absent row is indistinguishable from an
  oversight and this was a decision:

  - Its `tools` config section exposes only `view_image` and `web_search` — there
    is **no way to deny its editing path by name**, the way every other row's
    mechanism works.
  - Its only containment axis is `sandbox_mode`, and neither usable rung is the
    reviewer's tier. `read-only` also requires approval to *run commands or access
    the network*, which removes the tests and the `gh` a review is made of;
    `workspace-write` denies nothing at all, leaving the frictionless edit path
    that #462 exists to close wide open.

  So a codex reviewer would be a reviewer class with the #462 guarantee quietly
  absent — a gate-integrity hole rather than a feature. #267 stays open for it: if
  codex grows a tool-level deny, the change is one row plus a spawn adapter.

  **Why not the CLI's `plan` permission mode? (the "auto deny rule" flash, #79)** A human
  reviewing the planner's first boot caught a message about an "auto deny rule" and asked
  the obvious question: should the planner spawn in claude's `--permission-mode plan`
  instead of Auto + deny rules, and would plan mode still let it talk to the orchestrator
  over MCP and post its plan via `gh`? Both were investigated against the CLI docs (no live
  agent was spawned — reasoning is from `claude --help` and
  [permission-modes](https://code.claude.com/docs/en/permission-modes.md) /
  [permissions](https://code.claude.com/docs/en/permissions.md)):
  - **Plan mode would deadlock this planner.** Plan mode is read-only *and* built around an
    **interactive** hand-off: Claude researches, presents a plan, and then *asks the human*
    how to proceed (approve→auto, approve→acceptEdits, keep planning, …). There is **no
    documented non-interactive / auto-approve** path. Our planner pane has **no human** —
    so it would sit forever at the approval prompt. Worse, the two things the planner exists
    to *emit* — the loomux **MCP `report`** and the **`gh issue comment`** plan — are exactly
    the calls plan mode stops to prompt on before running them: in plan mode "permission
    prompts still apply as they do in Manual mode", and a mutating shell like `gh issue
    comment` is not a read, so each raises a **real-time approval prompt** — which, in a
    human-less pane, is simply never answered. So plan mode does not just add a prompt; it
    blocks the deliverable. **Copilot's `--plan` / `--mode plan` is the same shape** (an
    initial mode a human reviews before switching to interactive/autopilot), so switching
    CLIs doesn't buy a headless plan mode either.
  - **So the planner keeps a closed-by-default mode + structural deny rules** — which is
    the *autonomous* equivalent of plan mode's intent: read-only research, but free to
    emit its plan and report and then exit without waiting on anyone. To make that hold
    with **no human in the pane**, a `read_only` planner is now launched **unattended
    regardless of the group's `auto_ops`** (`unattended = auto_ops || read_only` in
    `build_agent_command`, applied to **both** CLIs): on Claude, `dontAsk` (#465 — see
    above; a pre-approved `Bash(git *)` / `Bash(gh *)` allowlist plus Claude's own
    built-in read-only Bash set, nothing else) — before #465 this was `auto`, Claude's
    *native* Auto permission mode, which additionally routed anything unlisted through a
    background safety classifier; on Copilot, `--autopilot --allow-all-tools
    --allow-all-paths` — so exploration, `gh issue view`, and the `gh issue comment`
    plan never prompt, with edits + `git commit`/`git push` denied on both (deny takes
    precedence over `dontAsk`'s allow rules / `--allow-all-tools`).

    - **Copilot autopilot mode, and why groups DO enter it (#101 delta).** Reading the
      installed Copilot bundle (v1.0.68, `app.js` + the `runtime.node` prompt strings) settled
      what autopilot *mode* changes beyond the idle auto-continue loop: it injects an extra
      **system-prompt** block, gated on `p.autopilotActive` (`_e = p.autopilotActive ?
      promptsCliAutopilotInstructions(...) : ""`), reading *"Autopilot mode is currently
      active … persist autonomously to complete the user's task … continue executing without
      waiting for user input … The user may not even be present."* Without it the agent keeps
      the `ask_user` tool (gated by the `ask-user` feature flag, **not** by mode) and its
      interactive framing — it will describe itself as interactive and may pause to ask. For an
      unattended, loomux-driven worker that autonomy directive is exactly what we want, so the
      **group** copilot posture is `--autopilot --allow-all-tools --allow-all-paths`
      (`COPILOT_GROUP_AUTOPILOT_FLAGS`).
      **#364 update:** the **single-pane** posture used to stay
      `--allow-all-tools --allow-all-paths` (`COPILOT_UNATTENDED_FLAGS`, no `--autopilot`) on
      the reasoning that a human at the pane doesn't need autopilot framing — but the human's
      report was that the launcher's Autopilot checkbox should mean true autopilot mode on a
      single pane too, same as a group worker. So `single_pane_autopilot_flags("copilot")` now
      returns `COPILOT_GROUP_AUTOPILOT_FLAGS` verbatim (not a divergent string — same atom, no
      drift). Since a solo pane never receives a programmatic kickoff (`Role::Solo` "never
      receives a kickoff" — the human types their own first message), nothing in the group
      path's `deliver_prompt` confirm exists to answer the resulting dialog for it; a dedicated
      `OrchRegistry::confirm_solo_copilot_autopilot` watcher is started right after the pane's
      pty spawns (`orch_confirm_solo_copilot_autopilot`, independent of channel-tools/`soloBind`)
      and runs the SAME `confirm_copilot_autopilot_dialog` primitive with a far longer,
      human-paced wait (`SOLO_AUTOPILOT_DIALOG_WAIT`, 10 minutes, vs. the group path's
      `AUTOPILOT_DIALOG_WAIT`, 12 seconds tuned to loomux's own near-instant kickoff Enter).

    - **Answering the consent dialog deterministically.** `--autopilot` makes Copilot open its
      "Enable autopilot mode" dialog at startup (menu: *Enable all permissions (recommended)* /
      *Continue with limited* / *Cancel*; the recommended item is default-highlighted at
      `initialIndex` 0 and Enter selects it). Group workers *already* reached autopilot mode
      historically — but only because the kickoff's own Enter happened to land on this dialog,
      a collision that also intermittently **swallowed the kickoff** (the lost-prompt incidents
      #99's echo-retry was papering over). We now do it on purpose: for a freshly spawned
      unattended copilot agent, `deliver_prompt` runs `confirm_copilot_autopilot_dialog` after
      the readiness wait and **before** any paste — it watches the pane tail for the dialog
      (`copilot_autopilot_prompt_detected`, anchored on the title *and* the enable option so
      prose can't trip it) and sends one `Enter` (`COPILOT_AUTOPILOT_CONFIRM_KEYS`) to accept
      the default, then lets the TUI repaint. The brief is pasted only afterward, so it can
      never collide with the dialog. Fail-soft: if the dialog never appears within
      `AUTOPILOT_DIALOG_WAIT` (Copilot changed the flow, or consent was pre-recorded), the
      confirm is a no-op and delivery proceeds. The human's group-level auto-ops choice is the
      consent — loomux is answering a dialog on behalf of an operator who already opted in.
      The confirm is gated to a **kickoff** (`Delivery::FreshKickoff` OR, since **#364**,
      `Delivery::ResumeKickoff` → `Delivery::confirms_autopilot_dialog` →
      `should_confirm_copilot_autopilot`): mid-session follow-ups/steers are long past boot and
      skip the watch rather than eat its fail-soft wait on every delivery. **#364 correction:**
      resume used to skip the confirm too, on the assumption that a resume restores
      allow-all/autopilot from Copilot's session event log so the dialog would never reappear —
      the human's report was that this assumption is false (the dialog does reappear, or
      autopilot isn't restored), so a resumed kickoff now confirms exactly like a fresh boot
      does.

    - **Accepted tradeoff: the solo watcher's wider false-positive window (#364 review, N1).**
      The group path's confirm only ever watches for `AUTOPILOT_DIALOG_WAIT` (12s), starting
      right after loomux's own deterministic kickoff Enter — a narrow window with almost no
      chance of a stray match. The solo watcher (`confirm_solo_copilot_autopilot`) instead polls
      the HUMAN's live terminal for up to `SOLO_AUTOPILOT_DIALOG_WAIT` (10 minutes) after spawn,
      because the dialog-triggering submit is the human's own first message and there is no
      lower bound on how long that takes. For the whole 10 minutes, ANY output in that pane that
      happens to contain both `copilot_autopilot_prompt_detected` anchor substrings (case-
      insensitively: "enable autopilot mode" and "enable all permissions" — e.g. the human
      pastes prose describing the dialog, or an agent reply happens to quote both phrases) would
      trigger loomux to inject an unsolicited `Enter` into that pane. This is a strictly wider
      false-positive blast radius than the group path ever had, and it is an **accepted cost**
      of AC#1 (a single pane must not launch into true autopilot mode with nothing able to
      answer its dialog) — not an oversight. The detector itself (`copilot_autopilot_prompt_detected`)
      is deliberately NOT being tightened in response: it cannot be re-validated against a live
      Copilot build in an agent session (CLAUDE.md constraint 3), and a tighter match that
      starts *missing* the real dialog is strictly worse than an occasional spurious Enter — a
      missed dialog leaves the pane silently stuck at "Continue with limited permissions"
      instead of true autopilot, while a spurious Enter is at most a wasted keystroke the human
      notices and can redo. Human live-validation for this PR should include watching for a
      spurious Enter landing in the solo pane during the watch window, not just confirming the
      real dialog gets answered.
    Previously a planner in a **non-auto_ops** group got the interactive preset (`acceptEdits`
    with no git/gh allowlist on Claude; plain interactive mode with no allow-all on Copilot),
    so its very first `gh`/explore call would have prompted into the void — a latent deadlock
    this fixes **on both CLIs**. Workers/reviewers are untouched: without `auto_ops` they
    still gate ops through the interactive preset.
  - **The flash itself was ours, not alarming.** It was Claude Code's own startup warning
    for a **malformed** deny rule: we passed both `Bash(git commit:*)` and `Bash(git commit *)`,
    on the mistaken belief that an unmatched spelling is silently inert. It isn't — `:*` is a
    valid wildcard only as a *trailing* suffix (`Bash(gh:*)` is fine); a colon in the *middle*
    of the command is not, so `Bash(git commit:*)` is discarded as malformed and warns at
    startup. The enforcing denial rests on the **space form** `Bash(git commit *)`, which is
    the canonical spelling and actually blocks commit/push; dropping the redundant colon-mid
    spelling removes the warning at its source (it never contributed to enforcement) rather
    than papering over it. **Direct answers to the human's two questions:**
    (a) No — the planner should *not* use plan mode; it would deadlock a human-less pane and
    block the plan/report. (b) In plan mode it could *not* reliably use the loomux MCP or post
    via `gh` unattended — each raises a real-time approval prompt no one is there to answer —
    which is the second reason we keep Auto + deny.

- **Per-role CLI + model.** `Guardrails` gains a per-role CLI (`orchestrator_cli`,
  `worker_cli`, `reviewer_cli`, `planner_cli`) and `planner_model`, alongside the existing
  per-role models. `agent_cli` stays as the **group default**: a per-role CLI that is
  empty inherits it, so old `group.json` (and the single-CLI launcher path) keep working
  unchanged. Resolution is centralized in `Guardrails::cli_for(role)` / `model_for(role)`,
  which every spawn site now calls instead of reading `agent_cli` directly — so the
  claude-vs-copilot decisions (session-id pre-assignment, copilot baseline/session watch,
  folder pre-trust, MCP-config shape, command adapter) are made **per agent** rather than
  per group. Model fallbacks follow the role's *effective* CLI (`default_model`: copilot →
  `auto`; on Claude the reasoning roles orchestrator/planner → the strong tier, worker/
  reviewer → the mid tier). All new fields persist additively in `group.json` (coexisting
  with #56's live `max_agents` patch, which only touches that one key), and are read back
  with empty-string defaults so a resume is forward/backward compatible.

- **Enforcement.** The group-default `agent_cli` is still coerced to a supported CLI in
  `clamped()` (legacy path), but per-role CLIs are **validated at spawn** rather than
  coerced: an unsupported per-role CLI (only reachable via a hand-edited `group.json` —
  the launcher offers only supported CLIs) makes `spawn_agent` return an error naming the
  supported set, instead of silently downgrading the role to Claude.

- **Launcher.** "Orchestrator + workers" mode renders a CLI select + model picker per
  role, seeded from the group-default *Agent* select and independently overridable; a
  role's model list follows its own CLI's suggestions (curated list, merged with the CLI's
  own reported models once the availability probe returns). Every distinct role CLI is
  PATH-checked before launch.

- **Prior art.** Pre-existing PR #5 (`feat/agent-profiles`) explored the adjacent idea of
  configurable, per-agent personas loaded from workspace files. This work is implemented
  fresh on the current base (which post-dates #5 by months) and takes a narrower,
  role-based shape — a fixed planner role plus per-role CLI/model — rather than #5's
  free-form profile files; the only thing carried over is the general direction of
  differentiating agents per role. #5's disposition (close vs adapt) is the human's call.

## `get_output` composed-screen capture (#520)

`get_output` is the orchestrator's only direct look at a pane, and it was the most expensive
tool in the set. It read the pty's raw ring, deleted the escape sequences (`strip_ansi`), and
collapsed *consecutive identical* lines (`collapse_repeated_frames`, #480/#496 PR-E). Against a
CLI that redraws whole lines that works. Against a modern TUI it does nothing at all, and Claude
Code v2.1.x is the worst case observed live (2026-07-30): a 30-line read returned ~12k tokens of
interleaved animation residue, and two calls burned ~24k tokens — 15% of a fresh compact.

**Why the line collapse could never have caught it.** Three independent reasons, all structural:

- The TUI repaints **partial lines** — cursor addressed to one column, a few cells rewritten.
  Delete the escapes and consecutive frames concatenate into a single ever-growing string, so
  there are no two "lines" to compare in the first place.
- The status **verb cycles** (`Shenaniganing…` → `Roosting…`) and a token counter **ticks**, so
  even a full-line repaint is never byte-identical, and `spinner_frame_core`'s stable-core trick
  (deliberately conservative — it strips one leading glyph and one trailing paren group, nothing
  fuzzy) has no stable core to find.
- The input box holding the delivered prompt sits inside the repainted region, so the prompt text
  itself multiplies 5-10x across the capture.

Retuning the dedup cannot fix any of these; the axis is wrong. Deleting escape sequences throws
away the one piece of information that makes the stream readable — **where each write landed**.

**The fix: replay, don't dedup.** `orchestration::termgrid::render_screen` is a small,
dependency-free VT replay: feed it the raw ring plus the pane's real geometry (`PtyManager::size`)
and it returns the composed screen — rows that scrolled off the top, then the rows currently on
screen. A redraw is only noise because it *overwrites* something; replaying the writes makes the
overwrite happen, exactly as it does in the human's terminal. Three hundred spinner frames painted
over each other become the one frame that is genuinely there.

This is CLI-generic in the strongest available sense (CLAUDE.md constraint 8): the module knows
nothing about spinners, verbs, or any CLI's vocabulary — only about ECMA-48. That is also why the
issue's fallback shape (similarity-based frame collapse plus stripping animation artifacts as a
pattern class) was **not** implemented: it was the "failing that" branch, the replay landed, and
adding a heuristic on top would reintroduce exactly the over-collapse risk #501's review round
spent its time closing, for output that is already one screen.

**Deliberate limits.** Not a terminal emulator: SGR/colour, character sets and wide-character
widths are parsed only far enough to be skipped, so a double-width CJK glyph occupies one cell
here and two in the pane (cosmetic in a monitoring read). Replay starts blind — the ring is a
256 KB *tail*, so it begins mid-stream against a blank grid — which is fine for absolute-addressed
paints and clamps for a relative move off the top edge. Because that boundary routinely lands
mid-codepoint, a malformed UTF-8 byte costs exactly one byte and resyncs, never the width its lead
byte claimed: skipping the claimed window would eat the valid characters sitting behind a single
bad byte, at the head of nearly every capture. Scrolled-off rows evict from a deque at the oldest
end, so a pane spamming bare newlines cannot buy O(cap) of shifting per scrolled row with one
`get_output` call. And the alternate screen is parked rather than cleared, so a pager that exits
leaves the pane reading as the human sees it.

**Blast radius.** `strip_ansi` is unchanged and still serves every other caller (`box_holds_paste`,
`prompt_wait_detected`, the compact/menu detectors, #522's confirmation sampling). This changes
what the orchestrator *reads*, never what loomux *decides*. The `last_exit_tail` fallback (#281)
was captured as already-stripped text — there is no escape stream left to replay — so it keeps the
line-collapse treatment alone.

**The byte cap.** Independently of all the above, `format_output_tail` now bounds the *payload*:
`OUTPUT_TAIL_MAX_BYTES` (8 KB), whatever `lines` asked for, keeping the newest end and stating on
the dropped line how many bytes went. `lines` bounds distinct content lines; bytes bound the
reply — whichever binds first wins. A pane rendering a 200-column build log can put several KB on
a single line, and 500 of those is a six-figure token bill answering a small question; no amount
of redraw collapsing bounds that, because none of it is a redraw. The marker reports the fact
(bytes dropped, cap hit) rather than characterising the dropped bytes as "animation residue" the
way #520 proposed: by the time the cap runs the replay has already removed the redraw churn, so
what is left is as likely to be a legitimately enormous log, and labelling it residue would be a
claim the code cannot back.

## Task-board dependency links, derived readiness, atomic claim (#582, slice A)

The board persisted a *display*, not a plan. `Task` carried no relationship at all, so the
ordering between items lived in exactly two lossy places — the orchestrator's context window and
the prose blob in `set_state` — and "what's unblocked right now" was re-derived from prose after
every compact or restart. `blocked` was a status with no object (the board could say a task was
blocked, never *by what*), and `assignee` was a plain field written by whoever called
`upsert_task` last, so nothing structurally stopped two workers being pointed at the same item.

Slice A is the model and the backend; the board UI (chips, ready affordance, dep editing) is
slice B, in its own section below — nothing in *this* section is visible to the human on its own,
since a dependency set through the orchestrator's tools shows up on the board only once B renders
it. The one behavior change a human could reach with A alone is delete-strip, below.

**Two flat fields, not a typed link array.** `Task` gains `deps: Vec<String>` (blocking) and
`related: Vec<String>` (annotation), both ids of tasks on the *same* board. A shared
`links: Vec<{kind, target}>` was rejected because the two kinds don't share semantics — deps are
cycle-checked and drive readiness, `related` is existence-checked and inert — so a common struct
buys nothing and forces every consumer to filter by kind. It has no migration advantage either: a
hypothetical third kind is an additive `#[serde(default)]` field whichever shape is chosen. Deps
do not cross groups (a cross-group edge would hand one group's board a handle on another's
lifecycle; cross-workspace coordination is what channels are for, #271).

**Persistence is additive and inherits its atomicity.** Both fields are
`#[serde(default, skip_serializing_if = "Vec::is_empty")]`: a pre-#582 `tasks.json` loads with
empty links, and a board that uses none rewrites without gaining either key — no version field,
no migration pass. Every mutation stays inside the existing `tasks_lock` + `atomic_write` path
#133 mandated; this adds no new write path. One compat edge, stated rather than silent: an
*older* loomux binary reading a newer `tasks.json` ignores the unknown fields and drops them on
its next write. That is acceptable for a local single-app file precisely because the dep-free
case is byte-identical — the only thing at risk is links a newer build wrote, and only if the
human downgrades mid-group. `pre_582_boards_load_unchanged_and_link_free_boards_stay_link_free`
pins both directions against the file text.

**Readiness is derived, never a status write.** `TaskSummary` (the `list_tasks` row) gains
`deps`, `related` and `ready: bool`, computed at read time as `queued` ∧ every dep `done`.
Nothing ever auto-flips a `status` from dep state: a queued task with unmet deps simply reads
`ready: false`. That is deliberate under lessons.md's *any suppression driven by a fallible
signal must be BOUNDED* — a derived read-time flag cannot wedge anything and so needs no bound,
where a mechanism that wrote `blocked` on the task's behalf would need an answer to "what if it
never clears". Only `done` satisfies an edge (`pr`/`human-testing` is work the human hasn't
signed off), and `related` never participates. There is no `deps_unmet` field: `list_tasks`
returns the whole board in one response, so with the dep ids and every dep's own `status` in the
same payload, *which* dep is holding a task is directly readable.

Readiness needs board context, which is why the projection is board-at-a-time
(`board_summaries`) and `task_summary(t, ready)` takes the flag as a parameter instead of
pretending a lone task can compute it. `deps`/`related` on the row are **ids only** — #245's size
constraint (a live board hit 228,577 chars for 70 tasks and blew MCP result limits), restated
here because a dependency graph is exactly the shape that tempts an expansion into titles or
nested tasks.

**Three rules keep "every link names a live task" true.** (1) Write-time validation: ids are
trimmed, blanks dropped, deduped first-wins, self-links refused, and every id must name a live
task. (2) Cycle rejection on `deps`, DFS from the edited task inside the lock, with the error
naming the path (`t-1 → t-3 → t-2 → t-1`) — an agent-authored cycle is always a bug, and allowing
one would cost real semantics ("is a task in a cycle ever ready?"). Only the edited task can
close a new cycle, since every other edge was acyclic when written, so the search starts there
rather than sweeping the board. `related` is never cycle-checked: a mutual see-also pair is
meaningful. (3) Delete-strip: deleting a task removes its id from every remaining task's links in
the *same* locked write, on all three delete paths (single, multi-select, delete-all-done), with
the rewritten task ids in the audit row. Refusing the delete instead was rejected as fighting the
human's authority over a board they hand-edit; leaving the ids dangling was rejected because a
deleted dep would then be indistinguishable from a typo. `unmet_deps` treats an unknown id as
**unmet**, never satisfied — with those three rules a dangling id can only come from a
hand-edited file, and reading a typo as "satisfied" would silently unblock work.

**Claim is a flag on `upsert_task`, not a new tool.** `claim: true` (id required) refuses unless
the task is still `queued`, is unassigned or already held by the same agent, and has every dep
`done` (the error lists the unmet ids); on success it sets assignee + `in-progress` in one locked
write, audited as `task-claim` rather than `task-upsert` so the record says *why* the assignee
moved. Re-claiming a task the same agent already holds is an idempotent no-op — the retry this
has to survive is "did my claim land before the compact?", and erroring there would push the
orchestrator back to the plain assignee write the guard exists to replace. The guards run
holder-before-status, which the tests caught as a real difference rather than a style
preference: a task another agent holds is *also* past `queued`, so checking status first
answered the double-assign case — the one case this guard exists for — with "status is
in-progress", never naming who actually had it. A claim that also
passes a different `status` is rejected rather than silently overridden, because one of the two
arguments would otherwise be a lie.

A separate `claim_task` tool was rejected: per the add-orch-tool checklist a new tool costs six
layers, and `upsert_task` is already orchestrator-only, so the authz story is identical. An
optimistic-concurrency version token on `Task` was rejected too — there is one process and all
writers serialize on `tasks_lock`, so a CAS token is machinery without a race. The failure mode
this actually closes is *semantic*: a post-compact re-read handing already-assigned work to a
second worker. Plain (non-claim) upserts keep last-writer-wins, unchanged, which is what the
human's board keeps using — `orch_upsert_task` gains `deps` (validated identically, so a
human-authored cycle surfaces through the board's existing error path) but deliberately not
`claim`: the board's authority is the human's, not a queue discipline. `related` is not on the
human's command either, since slice B renders it read-only; adding it later is additive.

**MCP shim stays thin.** `mcp.rs` gains an array arg helper and a bool arg helper and passes them
through — absent means "leave untouched", `[]` means "clear", and a wrongly-typed argument is an
**error**, not a silent skip: a caller that passed `"t-3"` where an array belongs must not be told
its write succeeded. All the rules live in `OrchRegistry`, so the human board's separate path
(`orch_tasks`/`orch_upsert_task` → same registry methods) gets them for free.

**Template guidance is part of the work**, since a field no orchestrator sets is dead weight:
`orchestrator.md` now says to encode a plan's ordering as `deps` instead of `set_state` prose, to
read "what's startable" off `ready: true` rather than re-deriving it, to assign with `claim: true`
(a refusal is the board telling you the task is taken or blocked), and that `blocked` is for
blockers *outside* the board. `planner.md` asks for the serialize/parallel structure of a worker
split to be stated explicitly per slice, since that is what the orchestrator turns into deps.

Not built, deliberately (the issue's own "do not reimplement Beads"): git-backed storage (this
board is per-group, single-process, serialized on one lock and already atomically persisted — a
second persistence mechanism for a race that can't occur), hash ids (ids are minted under one
lock on one board), a link-type zoo (`supersedes`/`discovered-from`), a ready-work query
language (one derived bool on rows already there), and memory-decay features (`cap_task_notes`
owns board boundedness). No new dependencies, front or back.

## The board's side of the dependency graph (#582, slice B)

Slice A made the structure persistable; slice B is what the human actually sees. Without it a
dependency set through the orchestrator's tools is invisible on the board — the issue's fourth
proposed step ("a task blocked by an unfinished dep reads visually distinct from one merely
sitting in `queued`") is a rendering claim, and only this slice can make it true.

**The board derives readiness itself, and that duplication is the design.** `ready` is a
`TaskSummary` field, and `TaskSummary` is the MCP `list_tasks` row — the *orchestrator's* path.
The human's board reads full `Task`s over `orch_tasks`, a separate command, so it computes
readiness from `deps` + the statuses already in that payload (`taskboard.ts`: `depState`,
`unmetDeps`, `isReady`, mirroring `dep_satisfied`/`unmet_deps`/`task_ready`). The alternative —
adding a derived flag to the human command's wire shape — would be a new contract for something
the board can compute exactly from data it already has, and it would still need the frontend
rule for the chips (`met` vs `unmet` per dep is not a single bool). Both sides are pinned by
their own tests, including the case that would matter if they drifted: only `done` satisfies an
edge, and an id naming no live task counts as **unmet**, never satisfied.

**The blocked/ready pair is the whole visual point, and it is deliberately quiet.** A `queued`
task with an unfinished dep recedes (opacity), and a `queued` task whose deps are all done gets a
`▸ ready` chip next to ▶ Start. No new accent color was introduced: the board already spends
amber on "waiting on YOU" (`awaiting-human`) and blue on live work, and a third accent competing
with those would cost more than it explains — the chips on the line below name *what* a row is
waiting for, which is the part prose in `set_state` could never do.

The ready mark is gated on the **board** using deps at all (`boardUsesDeps`), not on the task
having them. Every queued row on a dep-free board is trivially ready, so an ungated mark would
land on every queued row of every existing board and carry no information. Gating per-*task*
instead was rejected for the opposite reason: once some task declares a dep, a plain queued row
genuinely is startable, and marking only the linked rows would leave the rest ambiguous.

**The links line exists only when there are links.** A row with no `deps`/`related` (and no open
picker) renders exactly as it does today, so adopting this feature costs a dep-free board no
vertical space at all — worth stating because the board is the human's primary surface and its
row height is its density budget. A dep chip carries **✓ met / ✗ unmet / ⚠ missing**; `missing`
is its own state rather than folded into unmet because its cause and fix differ — the backend
validates ids on write and strips them on delete, so a link naming nothing can only come from a
hand-edited `tasks.json`, and the answer is "remove it", not "wait for it".

**Editing goes through the path that already exists.** Every add/remove sends the WHOLE `deps`
array on the board's existing `orch_upsert_task` invoke (array args are replace-or-untouched,
never a delta), so it inherits that path's validation, its audit row, and its error toast. The
picker therefore does **not** pre-filter choices that would close a cycle: the backend rejects
those inside its lock with an error naming the path, and a frontend cycle walk would be a second
copy of an authoritative rule that could only ever disagree with it. Letting the human pick a
cycle and be told why is strictly better than a picker that silently omits an option for reasons
it can't explain. `related` is rendered read-only for the matching reason: `orch_upsert_task`
takes `deps` and not `related` (slice A), so an editing affordance here would have nothing to
call.

**Picker state lives in the view model, not the DOM.** The board re-renders on every
`orch-tasks-changed` event, and a `<select>` is not an `INPUT`/`TEXTAREA`, so the existing
mid-edit refresh deferral (`isEditing`) does not cover it. Holding the open picker's task id in
the view (`picking`, pruned on refresh like the multi-select) means a background refresh
re-renders it instead of closing it mid-choice; a separate one-shot `pickingFocus` flag means
only the render that *opened* it takes focus, so a later refresh can't yank focus back from
wherever the human has moved.

Frontend logic that needs tests is DOM-free per the repo's convention (`taskboard.ts` +
`test/taskboard.test.ts`, `node:test`); the DOM wiring in `tasksview.ts` is validated by hand, so
the PR lists what a human pass has to cover rather than simulating a DOM.

**The directive ledger is not this, and the two must not be merged** — #582's fourth open question,
recorded here because it is a decision a future author has to find *before* deciding to unify them.
The ledger (`note_directive`, *docs/orchestration.md* §compact) persists **prose**: a directive as
the human phrased it, per agent, append-only, replayed verbatim into that one pane's post-compact
re-grounding notice. The board persists **structure**: validated ids, group-scoped, read by tools
and rendered above. That they survive the same event — a compact — is the whole trap; it is the
only thing they have in common. Unifying them would have to either force a human's sentence through
a schema whose one option is to reject it, or let unvalidated free text into the readiness
derivation, and it would turn a per-agent record into a group-shared one where every agent's
directives land in everyone else's context. They are complementary and deliberately kept apart: a
dep says *t-5 waits on t-3*, a ledger line says *why the human wanted it that way*. The relationship
to aim for is that a reader of one can find the other — never that either becomes the other.

## Copilot's two permission surfaces, and why loomux writes both (#802)

A block-spawned Copilot delegate came up with the loomux MCP server listed and none of its
tools permitted. Three candidate causes were checked against the reference; two of them
turned out to be the same defect wearing different clothes, and the third was a red herring
worth recording so it isn't re-investigated.

### The flags: one occurrence, not one per pattern

`--allow-tool` and `--deny-tool` are documented to take **"a quoted, comma-separated
list"**, and neither is annotated *"(can be used multiple times)"* — while `--add-dir`,
`--attachment`, `--add-github-mcp-tool`, `--add-github-mcp-toolset` and
`--disable-mcp-server` all are, and `--secret-env-vars` carries both annotations at once.
The reference is precise about this distinction, so its silence here is a signal.

loomux emitted them repeated: `--allow-tool loomux`, then the attended `shell(git:*)` /
`shell(gh:*)` pair, then one occurrence per `allow:` pattern a workflow block declares. If
Copilot resolves repeated occurrences last-wins rather than by accumulating, the MCP grant
— emitted **first** — is the one that disappears. That is the reported symptom exactly.

This is not a claim to have caught the parser doing it: whether Copilot accumulates or
overwrites is undocumented, and settling it needs a real `copilot` run, which CLAUDE.md
constraint 3 reserves for a human. The point is that the single comma-separated occurrence
is correct under *either* reading, so it needs no theory of the parser. The repeated form
did. (This is the same shape as #610 on Claude's `--allowedTools`, reached through a
different CLI: a grant that reads as present on the command line but isn't in effect.)

It also answers the "workflow blocks specifically" half of the report. Tracing the spawn
path end to end, a workflow-block spawn and a built-in-roster spawn go through the *same*
`build_agent_command_ex`/`build_agent_argv_ex` with the same `guardrails.auto_ops` and the
same `role.containment()` — there is no block-specific arm. The **only** copilot-argv
difference a workflow block introduces is extra `--allow-tool` occurrences from its
persona's `allow:` patterns.

### The store: `permissions-config.json`, not `config.json`

`pre_trust_copilot_folder` wrote `trustedFolders` into `~/.copilot/config.json`. That pair
appears in **no** page of Copilot's current reference. `config.json` is now *"internal
application state that is managed automatically by the CLI, including authentication
data"*, whose user settings *"are automatically migrated to `settings.json`"*;
`trustedFolders` is absent from that description, from `settings.json`'s full settings
table, and from `permissions-config.json`'s full schema. What the reference **does**
describe is `permissions-config.json` — *"Saved tool and directory permissions per
project"* — and the allow/deny how-to says outright: *"Any directories you grant access to
are saved to the same file."*

So loomux now writes the documented store, with two grants straight off its schema:

- `locations.<key>.allowed_directories` — *"Extra directories that the path gate can access
  for this location."*
- `locations.<key>.tool_approvals` `{ kind: "mcp", serverName: "loomux", toolName: null }` —
  *"Approves one MCP tool, or every tool on the server when `toolName` is `null`"*, with
  *"`serverName` must match the configured MCP server name exactly."*

That second one is the durable form of the grant `--allow-tool loomux` already makes on
argv, and its absence is what the report describes. It widens nothing outside loomux: an
approval names a *server*, and a session that never loads loomux's
`--additional-mcp-config` has no such server to invoke.

**The key is the git root, not the agent's workdir.** The reference keys this map by *"the
Git root used for permission scoping"* and states *"Linked worktrees resolve to the main
repository root, so they share permissions with the main worktree"* — so a worker in a
dedicated worktree looks its approvals up under the repo it was cut from. The worktree
still needs the path gate, which is what it contributes to `allowed_directories`. An
existing key naming the same directory is **reused**, never duplicated: Copilot loads
exactly one key (*"If the key doesn't match, the saved approvals won't apply"*), so a
second spelling would strand the user's own approvals under whichever one Copilot didn't
pick. For the same reason the legacy extensionless `permissions-config` filename is edited
in place when it is the live one — creating the `.json` beside it would make Copilot stop
honouring the legacy file and silently discard every approval the user had saved.

**"The same directory" is the host's rule, not Windows's.** loomux ships macOS and Linux
builds, and the first cut of this writer normalized every key the Windows way — `/` folded
to `\`, compared case-insensitively. Off Windows that is not a normalization but a
corruption: `/home/u/repo` became `\home\u\repo`, a key Copilot never matches while looking
up `/home/u/repo`. A permission write that silently does nothing is the very failure class
#802 is about, so the fix would have reproduced the bug on two of the three platforms
loomux ships.

The reference states both halves of the rule in one sentence, and only the first half is
Windows: the CLI *"compares paths case-insensitively on Windows, and compares paths
**case-sensitively on other platforms**"*. So case folds only on Windows — folding
everywhere would merge `/srv/App` and `/srv/app`, two genuinely distinct directories on
Linux — and `\` is rewritten only on Windows, because elsewhere it is a legal *filename
character* rather than a separator. A trailing separator is stripped on both, but never
down to an empty key: a bare root keeps its separator.

This is expressed as one `const HOST_IS_WINDOWS: bool = cfg!(windows)` feeding pure
`*_for(…, windows: bool)` functions, rather than as `#[cfg]`-split function bodies. The
reason is testability: a `#[cfg]` split compiles only its host's half, so the POSIX rule
would go unexercised on the Windows CI leg and vice versa — each platform's tests would
pass while proving nothing about the other. `path_keys_follow_the_hosts_own_path_semantics`
drives **both** shapes explicitly on every leg, plus a small host-semantics check that the
production wrappers agree with whichever half applies.

**The legacy `trustedFolders` write stays.** Absence across three reference pages is a
sourced absence, not a documented removal, and loomux cannot observe which Copilot build a
machine runs. Dropping it would trade a confirmed gap for a possible regression. Both
writes remain inside the `is_live_registry()` containment rule described above — the
permissions store is a second file under the same predicate, not a second exception to it.

**Both writes are `atomic_write`, not `fs::write`.** These are the *user's* files:
`permissions-config.json` holds every approval they have ever granted Copilot in any repo,
and `config.json` holds Copilot's authentication state. `fs::write` truncates before it
writes, so a disk-full or a crash mid-write leaves an empty or half-written file and the
user silently loses the lot — #133's failure verbatim (a disk-full `fs::write` truncated
`tasks.json` and destroyed the live board), and strictly worse here because the data is not
loomux's to lose and loomux cannot regenerate it. The grant stays best-effort at the policy
level — a failure means Copilot prompts as before, never a failed spawn — but a failure can
no longer be *destructive*.

### The red herring: block containment defaults

The third candidate — that a workflow block defaults to a containment forcing Copilot's
deny arm — does not exist. Containment is never declared per block: it is
`role.containment()`, derived from the block's `kind`, on both spawn paths. A `kind:
worker` block gets `Containment::None` whether it came from `workflow.yml` or the built-in
roster.

### What is still unverified

Whether a bare server name in `--allow-tool` matches on 1.0.77 was the report's leading
hypothesis; the reference refutes it — the tool-permission-patterns table lists
`SERVER-NAME` as a Kind in its own right (`MyMCP(create_issue)`, `MyMCP`) with the worked
example *"Allow all tools from a server: `copilot --allow-tool='MyMCP'`"*. The value is
unchanged; only its packaging moved. What remains genuinely open is the accumulate-vs-
overwrite question above, and (unchanged from #463) whether an unmatched `--deny-tool`
value warns, errors, or no-ops. Both need a live run a human owns.

## A `StrandedSubmit` marker is a repair, not a payload (#813)

The self-heal marker (#445 seam 3, #496 PR-C) is pushed to the **front** of a pane's
delivery queue, the drainer only ever drains the front, and its press was gated on
`should_flush_before_paste` — which requires that *no human has typed since our own
submit*. Every part of that was deliberate, and together they deadlock.

`human_input_block` (#518) re-arms for `HUMAN_INPUT_BLOCK_BOUND_MS` from the human's
**last** keystroke. So the marker's release condition is anti-correlated with the human's
own recovery: click into the wedged pane, press Enter to submit the stranded prompt, then
keep typing to talk to the CLI, and the marker is pinned at the head of the queue for as
long as you stay engaged. Everything queued behind it — every steering prompt — silently
never delivers, and the `attn_stranded` chip, which clears only on a **confirmed delivery
to that pane**, cannot come down either, because no delivery can happen. That is #813's
live incident: three orchestrator panes, a workstation lock, and a group that only a
restart recovered.

The fix is a reclassification, not a new mechanism. Every *other* queue entry carries text
that exists nowhere else; a marker carries an **Enter**. So the marker was the one queue
entry whose failure to fire cost *other* work, and nothing bounded it.
`stranded_marker_action` makes the precedence a value (`failed_arm_route`'s shape, for
`failed_arm_route`'s reason) with three retire cells and three holds.

### What a retirement is allowed to rest on

This is the part that took a review round, and it is the whole of the design.

The first cut retired on **"a keystroke landed after our submit, and `input_pending` is
false"**. That is wrong twice over, and both errors have the same root — reasoning about
*who typed* instead of about *where our text is*.

**It cannot tell a person from a phantom.** #518 exists precisely because a terminal
auto-reply can be misclassified as a keystroke, and a phantom's signature is exactly that
pair: a stamp, an empty occupancy counter. Worse, `BoundedOut` is reachable *only* with
`!box_pending`, so a retirement on that pair always fires strictly **before** the bound
could release. The first cut did not coexist with #518's bound; it made that bound
**unreachable from this path**, while writing `human-resolved` into the audit trail for a
pane no human had touched.

**And `!box_pending` is not "the box is empty".** `input_box_len` counts characters the
*human* typed (`note_user_input`); loomux's own paste goes out through `write_bytes`, which
never touches it. So a retirement there can happen while **our own prompt is still sitting
in that box** — and `deliver_now` does *not* abort when `flush_stranded_text` declines, so
the next queue entry pastes on top of it and the pre-Enter wait submits both prompts merged
as one. That is the #81/#84/#111 collision the stranded flush exists to prevent, re-opened
by the repair meant to help. The marker's head-of-line block had been *accidentally*
preventing it.

So a retirement now requires a positive [`BoxReading::NotHolding`] on our own recorded text
— the tail was long enough to have contained our paste, and does not. Only then does the
keystroke record pick the **name**, which is all it was ever fit to decide. `Holds`,
`Unverifiable`, and "no text on record" all fall through to the ordinary gates: nothing
retires on an absence of evidence, which is the one direction that could re-open either
hazard.

| reading | before | now |
| --- | --- | --- |
| ledger not `Some(false)` — nothing stranded | retry forever | `Retire(NothingStranded)` |
| `NotHolding` **and** a keystroke since our submit | retry forever | `Retire(HumanResolved)` |
| `NotHolding` with no keystroke on record | retry forever | `Retire(TextGone)` |
| `box_pending` — human characters outstanding | decline | `Retry(BoxOccupied)` — unchanged |
| `human_input_block` holds | decline | `Retry(BoxOccupied)` — unchanged |
| live question | decline | `Retry(Question)` — unchanged |
| clear | press | `Press` — unchanged |

That the ledger now carries the text at all is the enabling change:
`DeliveryOutcome::stranded_text` is written by the two paths that know it — the ownership
publish before the first Enter, and `record_aborted_preenter_outcome` — because the marker
itself carries no text, and without it nothing downstream can ask the only question that
licenses a retirement.

### What retiring cannot lose

A retirement means our text is **not in the box**. There is therefore no Enter left for the
marker to press that could have submitted it, and nothing for a later paste to collide
with. The case where our Enter would still have been right is exactly `Holds`, and that is
the case the marker keeps retrying — at which point the suppression is bounded by evidence
rather than by a timer, which is the release the repo's standing rule prefers
(#496/#513/#518): the same event that makes the press possible is the one that ends the
hold.

The residual is a pane whose text `Holds` indefinitely behind a gate that never clears.
There the queue does stay held — correctly, since the alternative is merging two prompts —
and #496 PR-C's badge plus `QUESTION_HOLD_STALE_AFTER`'s escalation are what tell the
human, exactly as they did before. What #813 removes is not that hold; it is the hold that
survived the human *fixing it*.

### The badge

`HumanResolved` is the one cell that clears the chip (`clear_stranded`, reason
`human-resolved`): a person demonstrably acted on the pane and our text is gone. `TextGone`
does **not** — our text left the box with nobody at the keyboard, which is
`StrandedBlocker::NotHolding`'s own situation ("never confirmed and its text is gone —
check the pane"), and clearing there would answer a question loomux cannot answer.
`NothingStranded` establishes nothing about the pane at all.

That decision has to be the **only** one, which is a second edit rather than a consequence
of the first. `note_hold` drops a badged pane's stranded note whenever an episode ends, and
#813 added a second ender — so `Retired` silently inherited a badge clear that the retire
arm had already decided against, and a `TextGone` badge came down behind its back. The
clear there is now restricted to `HoldObservation::Delivered`: a delivery is the pane
*proving* it can accept a write, and a retirement is by construction the case where it
proved nothing (that is the whole reason `Retired` is not `Delivered`). Ending the episode
and clearing the badge were one decision because until #813 only one observation did both.

## An explicit dismiss is a release the pane cannot supply (#825, M1)

`attn_stranded` is a **latched** map, and that is deliberate: a chip that goes down while a
pane is still wedged is worse than one that overstays. But line the `StrandedBlocker`
variants up against everything that clears one and five of them — `NotHolding`,
`Unverifiable`, `Exhausted`, `QueueFull`, `PauseSuppressed` — have no release at all except
a *later confirmed delivery to the same pane*. On a pane that goes idle there may never be
another delivery, so a finished worker can wear a stuck-prompt chip indefinitely. The
gesture a human would reach for does not help either: `orch_ack_attention` clears
`attn_reports`, not `attn_stranded`.

That is not a cosmetic complaint. A badge nobody can take down is a badge people learn to
ignore, and the classes above include the ones that matter most when they are real.

### The gesture is the evidence

`dismiss_stranded` releases the chip on the one signal that is neither an inference nor a
timer: the reader the badge exists for, saying "seen", with a gesture aimed at the chip
itself. Nothing about the pane is being claimed, so nothing can be claimed wrongly — which
is why this one release is valid for **every** class, including the two that bracket the
problem:

- `PauseSuppressed` describes a loss that already happened. It cannot become untrue, so no
  reading of the pane could ever release it.
- `Unverifiable` describes a box loomux could not read. Precisely because of that, no
  reading of the pane is trusted *to*.

Between them they rule out every automatic release, and the human is what is left.

### What it settles, and what it deliberately leaves alone

A dismissal changes exactly one thing: whether a warning is on screen. No hold is released,
no queue entry admitted, no Enter pressed — this is the safety line, because it keeps the
whole of #825 outside #813's blast radius. The chip's own tooltip carries the same two
claims ("takes the chip down only, it does not unstick the pane"), since a human allowed to
retire a warning on their own say-so is owed both halves; "dismiss" read as "resolve" is how
someone walks away from a genuinely wedged pane.

The one thing a dismissal does not get back: for `QuestionStale` / `HumanInput` the badge is
raised through a hold episode's `badged` one-shot, which has already fired, so within that
same episode the chip will not re-raise. That is accepted for an explicit dismissal rather
than papered over by re-arming the one-shot — which would answer one deliberate gesture with
a chip that comes straight back.

### Why not the focus ack, and why not an expiry

The cheapest-looking fix is to let `ack_attention` clear the stranded map too. It is
rejected: that ack fires on pane **focus**, which the human performs all day merely to type,
so it would take down chips nobody read — and on `Unverifiable` / `Exhausted` the chip may be
the only trace of a prompt still sitting unsubmitted in a box loomux could not read. The
same ambiguity is why the frontend offers the dismiss control only for `stranded`: the other
attention reasons are re-derived by every 3-second scan or already released by the ack, so a
dismiss control on them would visibly evaporate on the next tick and teach the human that
dismissing does not work — the complaint this exists to answer.

A timed expiry is rejected on the standing rule (prefer independent evidence over elapsed
time, #496/#513/#518): an expiry on an unattended machine clears the notice precisely when
nobody saw it, which is the case the badge exists for.

### A dismissed badge stays diagnosable

`clear_stranded` records the reason but not the class, and for the classes that claim the
box may still hold text, the class *is* the diagnosis. So a dismissal writes its own
`stranded-dismissed` line first — actor `human`, carrying the blocker token and how long the
chip stood — and then clears with `why: "human-dismissed"`. Two lines, on purpose: one
reports a human act, the other a state change, and a concurrent clear from some other
mechanism is then visible as exactly that rather than as a lost record. Letting a person
retire a warning is only defensible while what they retired survives the retiring.

The command takes an agent id and nothing else; the group is read from the registry. A chip
can only be on screen for an agent `attention_tick` emitted, so an id with no agent record
could never have been clicked, and refusing to accept a group as a path segment (constraint
6) is strictly narrower than trusting one.

### Not in this slice

The automatic releases for the three evidence-bearing classes are M2, immediately below;
`QueueFull`'s re-admission is M3, below that. This slice adds no automatic release of any
kind.

## The badge honesty check outlives the monitor that made it (#825, M2)

The issue's framing needs one correction, and it decides the shape of the fix.
`run_late_confirmation_monitor`'s `KeepWaiting` arm has kept a raised chip honest on every
5-second tick since #496 PR-C: it reads the box against our own stranded text, clears on a
genuine present→absent transition, and re-derives the chip's wording live. So the latch was
never *"no release logic exists"*. The release logic **dies with the monitor** at
`LATE_MONITOR_MAX_LIFETIME` (four hours), and on an idle pane nothing looks again — ever.

The latch is therefore two regimes of *observation*, not two regimes of *logic*, and the
honest fix is to hoist the check rather than invent a second one. `DeliveryOutcome::
stranded_text` (#813) already outlives the monitor, so every input this needs persists
already; there is no new bookkeeping anywhere in M2.

### One matrix, one writer, two observers

- `stranded_badge_release` is the decision: `Clear(why) | Reword(blocker) | Keep`.
- `OrchRegistry::apply_stranded_verdict` is the write.
- The monitor's `KeepWaiting` arm and `OrchRegistry::stranded_janitor_pass` are two
  *observers* that ask them the same question.

Two honesty checks that could drift is the failure mode a copy would have shipped: a chip
that clears under a live monitor and re-latches an hour later, or the reverse, with no
reader able to say which was right. The hoist also drags two cells into the monitor that
were not there before — #819's human-resolved clear, and the wording upgrade below — and
that is the point rather than a side effect. A chip must not mean one thing while a monitor
is alive and another an hour after it exits.

### The bar for a clear, per class

The three classes a pane reading can answer are `NotHolding`, `Unverifiable` and
`Exhausted`: each one's badge is a claim about *where our text is*, so a later reading of
the same box is evidence about the same question. A clear needs #819's `HumanResolved` bar,
unchanged and extended from the queued-marker path to badge-only panes — a positive
`BoxReading::NotHolding` on the text the ledger still records, **and** a human keystroke
since our own submit. The reason string is taken from `StrandedRetireReason::HumanResolved`
rather than spelled again, so the drainer's retirement and the janitor's clear cannot come
to mean two different things in a log someone is reading to find out where their prompt
went.

Each half is load-bearing:

- `NotHolding` alone is `StrandedRetireReason::TextGone`, which #819 declined to clear on —
  "our text left the box with nobody at the keyboard" is exactly `StrandedBlocker::
  NotHolding`'s own sentence, and clearing there answers a question loomux cannot answer.
- The keystroke alone is #518's phantom, which is why a stamp only ever *names* a release a
  box reading has already licensed.

Requiring both is also what contains the two residuals this inherits. #828's remaining
false `NotHolding` (a hard mid-word wrap) can only ever change a chip's *wording*, so a
clear takes two independent failures — a misread box *and* a phantom keystroke — which is
the same conjunction #819 already accepts one path over. A false clear on `NotHolding`
alone would be a clear on `TextGone`, which is the thing neither path does.

Where the reading is `NotHolding` and no keystroke is on record, the chip stays up and is
**re-worded** to `NotHolding`. `Unverifiable` says "we could not read the box" and
`Exhausted` says "a heal fired and the text still read `Holds`"; both are stale sentences
about a box we have now read, and a chip that overstates what loomux knows is the same
honesty failure as one that overstays.

Everything else keeps the chip. `Holds`, `Unverifiable` and "no recorded text" are three
different facts sharing one conservative branch — nothing is released on an absence of
evidence, the single direction that could hide an unsubmitted prompt behind a chip that
went away. `PauseSuppressed` and the queue and hold classes are not this mechanism's to
release at all: a box reading establishes nothing about them.

### Why the two observers are not symmetric

`saw_text_in_box` is a fact about the **observer**, not the pane, and it is the only input
the two differ on. The monitor watches one delivery continuously, so it can witness a
genuine present→absent transition — our text was in that box and now is not, so whoever
pressed Enter, the pane is not wedged. The janitor starts, by construction, on panes whose
monitor is already gone; it has no such memory, can never witness a transition, and so
passes `false` always and clears on the stricter bar instead. The transition clear is
unchanged by the hoist: it still fires for every class, and it still outranks the
keystroke-named clear, because where the text went is a stronger claim than who was at the
keyboard.

### A re-word is never a raise

Since M1, an absent `attn_stranded` entry can mean *a human dismissed the chip*. Every
badge-honesty write reads the note at the top of a pass and writes it back at the bottom, so
"there was a chip here a moment ago" is exactly the stale premise a dismissal invalidates —
and `mark_stranded` **inserts**. On the monitor's live re-check that was a live defect in
its most reproducible form: dismiss the "loomux is re-sending it" chip and it came straight
back on the next 5-second tick.

`reword_stranded` updates in place or does nothing, so an absent entry stays absent whatever
the timing. It also takes the blocker the caller judged and compares before it writes, so a
verdict about a chip another mechanism has since re-raised cannot overwrite the fresher
diagnosis. The janitor inherits both guarantees by going through the same writer, and it
never iterates anything but entries that already exist.

### Cadence and cost

The pass rides the existing attention loop (3s) every tenth tick, ~30s, rather than owning a
thread: it answers the same "does this pane still need the human" question that loop exists
to ask, just from the pane instead of from the registry. The cadence is divided because the
answer only changes when a human touches the pane, and the work reads a pty ring. 30s is a
choice, not a finding; the INV-4 bound that makes it defensible sits at the call site in
`start_attention` and rests on four things — no pty read at all unless a chip is up in one
of the three classes (`attn_stranded` is empty on a healthy session, and the pass returns
after one uncontended map lock); per badged pane one `Tier1Scan::for_paste` normalize plus
`Tier1Scan::widen`'s own capped ring reads and one keystroke-stamp read; one sixth the rate
at which the late monitor already takes the identical read on the same panes; and a worst
case bounded by the badged-pane count.

It is self-limiting in the sense #819 F3 used: the reading is what takes the chip down, so
the work ends the condition that schedules it.

### What it writes, and what it never writes

The janitor takes a chip down or re-words one. No hold is released, no queue entry admitted,
no Enter pressed, no byte written to a pane — the same safety line M1 states, and what keeps
the whole of #825 outside #813's blast radius.

A clear by inference owes its evidence the way M1's `stranded-dismissed` records the class
behind a clear by gesture. `stranded-cleared` alone says `why: "human-resolved"`, which the
drainer's own #819 retirement also writes, so a `stranded-janitor` line (blocker, reading,
`human_since_submit`) goes ahead of the clear: without it a human whose chip vanished could
not tell which mechanism took it down, nor what it read to decide. Only on an actual clear —
never per pass.

## The refused re-send is retried, not reasoned about (#825, M3)

`QueueFull` is the last of the five, and the only one whose badge is not a claim about *where
our text is*. It says loomux could not even **queue** the re-send `actuate_stranded` had
already decided to make, because the pane was at `queue::QUEUE_MAX_PER_PANE`. M2's matrix
therefore has nothing to say about it: no reading of that box answers "was the marker
admitted". The honest answer is to admit it once there is room.

So M3 clears nothing. A successful re-admission **re-words** the chip to the in-flight heal
wording (`None`) and hands the pane back to the machinery that owns every other marker — the
marker's own gates at the press (#819/#824), a confirmed delivery's clear in `deliver_now`,
#819's retirement reasons. It mints no `stranded-cleared` reason of its own because it takes
no chip down; the one line it writes is `stranded-readmit`, which is what explains the wording
change to whoever greps for it.

This is a **repair** gap as much as a badge gap, and the repair half is the larger one. Before
it, the Enter loomux had decided to press and could not queue was never pressed at all, however
much room opened afterwards, on any pane whose late monitor had exited.

### Why it is not a hook on the drain edge

plan-312 put M3 at `note_queue_capacity`'s `Full` → not-`Full` transition, beside
`announce_refusal_roster` (#658). That hook could never have admitted anything, and the reason
is a runtime fact a read-only plan could not see: every production caller able to produce that
edge — `pop_front_dequeued`, `pop_batch_dequeued`, `drop_superseded` — runs on the drainer
thread, which holds its `queue_draining` registration from `ensure_drainer` right through to
`commit_exit`. `stranded_admission_gate` answers `drainer-active` there, every time. (The one
other caller, `drop_queue`, is destroying the pane's queue.)

It is also the wrong moment on the merits, which is what makes this a relocation rather than a
workaround. `stranded_admission_gate`'s own doc already says what a live drainer means: a
delivery is queued for this pane, and *that* delivery's pre-paste `flush_stranded_text` presses
exactly the Enter this marker wants pressed. The moment nothing else will press it is the
moment the queue has gone quiet and the drainer has exited — which is also the only moment the
marker can be admitted at all. The retry therefore observes the pane **after** the drain
instead of during it: the same hoist M2 performed out of the late monitor's lifetime, pointed
at the drainer's.

`the_drain_edge_itself_could_never_have_admitted_the_refused_re_send` pins this rather than
leaving it as prose. It drives the real edge — a real cap, a real refusal, a real
`pop_front_dequeued` — with the registration a real drainer holds, then releases it: same pane,
same queue, same chip, and only then does the marker go in.

*Rejected: teaching the gate that the drainer may admit its own marker between a
`pop_front_dequeued` and its next peek.* It is true that the drainer owns no entry at that
instant, and it re-opens #496 PR-C rev-47 B1 by construction — replacing a fused check-and-push
with a parameter every future caller of `note_queue_capacity` would have to get right — to buy
a repair that the very next queued delivery's pre-paste flush already performs.

### The gate, and what it costs

`queuefull_readmit_gate(blocker, depth, drainer_active)` is pure and total: `not-queue-full`
(every other chip is somebody else's release), `still-full` (the chip is still true), and
`drainer-active`, named with `stranded_admission_gate`'s own word so the pre-check and the check
under the lock cannot drift into two vocabularies. It is only a pre-check — the real one is
still the fused check-and-push inside `admit_stranded_selfheal` — and it exists so a declined
pass is *silent* rather than writing a refusal line every 30s. Its depth test mirrors
`push_stranded_front_locked`'s own `>=` rather than `queue::capacity_state`'s badge
classification, because it has to ask the question the push will ask.

The pass rides the divided attention tick M2 established, and is cheaper than the janitor by a
wide margin: no pty read at any point. One uncontended `attn_stranded` lock on a healthy
session, two map reads per badged pane, one `queues` critical section for the pane that passes.
It is self-limiting in the same sense — a successful re-admission re-words the chip off
`QueueFull`, so the work ends the condition that schedules it. The bound is stated at
`start_attention` beside M2's.

Two orderings are load-bearing and are commented where they are relied on. The re-word happens
**after** the admission: `admit_stranded_selfheal` calls `note_queue_capacity`, which treats a
chip whose blocker is `None` as nobody's badge and would stamp a depth badge over the in-flight
wording — `actuate_stranded` orders it the same way for the same reason. And the write goes
through `reword_stranded`, so a chip dismissed in the gap stays dismissed; the marker is still
the right thing to have queued, because a dismissal takes down a warning rather than
un-stranding a prompt.

### What it never does

It never presses Enter, and it never decides that pressing one is safe. It queues the marker
`actuate_stranded` had already decided on; every gate that governs the press is re-derived at
the press by `drain_stranded_submit`, against the pane as it is then. Nothing is released on
inference, and nothing about *when* loomux presses Enter changes — which is what keeps the whole
of #825 outside #813's blast radius.

## Risks / limitations

- Kickoff typing races CLI boot; a fixed delay (4s) + bracketed paste is used. If a
  kickoff is lost the orchestrator can re-`send_prompt` (both are visible in the pane).
- Watchdog silence is measured from pty *output*, so an agent that sits in a tight
  redraw/spinner loop (emitting bytes) without making real progress reads as "alive". The
  watchdog catches wholly-silent stalls (lost kickoff, blocked-on-input), not livelocks;
  those remain the orchestrator's / human's call via `get_output`.
- `gh` CLI must be installed/authed for the issue/PR workflow; templates degrade to
  local-only work when it's missing.
- Registry is in-memory: closing loomux tears down agent processes (kill_all) but live
  agents don't survive; durable state does. Resuming respawns fresh sessions on the old
  state. On **Windows**, "tears down" is a hard guarantee only because each pane child is
  enrolled in a kill-on-close **Job Object** — killing the pane closes the job and the
  kernel reaps the whole descendant tree. Without it, `TerminateProcess` hits only the
  direct child and descendants (wrapper→agent→bash/node) leak; the investigation for #78
  found exactly that (orphaned wrappers with live agents, a squatting vite). See
  [job-object-teardown.md](job-object-teardown.md). Unix needs no equivalent: the child
  is a session leader owning the pty as its controlling terminal, so dropping the master
  hangs up the terminal and the kernel delivers SIGHUP to the whole foreground process
  group.
- The compose strip (#43) makes steering collision-proof, but **direct** typing into the CLI
  box is only protected by the heuristic hold (A): a keystroke landing in the millisecond
  between the quiet-check and the paste, or a human who pauses mid-sentence past the 4s window,
  can still collide. Typing in the strip has no such window. The 90s starvation cap also means
  a marathon uninterrupted typing session eventually gets a report delivered on top of it —
  the cap trades a rare late collision for never starving reports.
