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
| `spawn_agent(name, kind, task, worktree?, branch?, base?)` | ✓ (guardrailed) | ✗ |
| `send_prompt(agent_id, text)` | ✓ | ✗ |
| `report(status, summary)` / `message_orchestrator(text)` | ✗ | ✓ |
| `list_agents()` | ✓ | ✓ |
| `get_output(agent_id, lines)` | ✓ | ✗ |
| `kill_agent(agent_id)` / `focus_agent(agent_id)` | ✓ | ✗ |
| `rename_agent(agent_id, name)` | ✓ | ✗ |
| `get_state()` | ✓ | ✓ |
| `set_state(state)` | ✓ | ✗ |
| `group_usage()` | ✓ | ✗ |
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
reviewers on it — a reviewer is read-only with respect to the repo's *content* (it never edits
files or pushes), but it is not read-only with respect to the clone's *checkout state*: `gh pr
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
convention is unaffected either way: it was never CLI-level-enforced to begin with (only a
planner's `is_read_only()` denies `git commit`/`git push` at the tool level) — it is, and
remains, taught in `reviewer.md`, and a dedicated worktree changes nothing about that; it only
gives the reviewer a workspace of its own to sit in while it does what it already does.

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
  one-task-per-worker, externalized memory — are stated **once**, in an `## INVARIANTS` digest at
  the very top, which the orchestrator is told to re-read at session start and after every
  compaction. Every body section then *stops restating them* and holds only the procedure and the
  why, cross-referencing the digest by number. That is what pays for the additions above: the
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
  Tauri commands; frontend `pauseGroup`/`resumeGroup`/`groupPaused`). While paused,
  `deliver_prompt` short-circuits *before* touching the pty — every kickoff, orchestrator
  prompt, and worker report is suppressed and audited (`prompt-suppressed-paused`), so agents
  finish their current turn and idle out rather than being killed. Nothing is queued or
  replayed: agents re-sync from the board/state on the next prompt after resume, which is the
  point. The flag is mirrored to a `paused` marker file so a pause survives an app restart
  (re-seeded in `create_group`).
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
  of delegate spawns doesn't crowd the orchestrator pane out of focus — reusing #46's
  existing minimize/restore plumbing (`Grid.minimize`) rather than a new "open into the
  dock" path, so a freshly-docked pane behaves exactly like one a human folded by hand a
  moment after it opened (including the "never dock the grid's last visible pane" guard).
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
  permanently; noted here so it isn't lost.
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
  counters. `run_notify_tick` = `poll_watches` + `notify_tick(now_ms(), …)`, called every
  `NOTIFY_POLL_INTERVAL` (30s) by `start_notify_poller`, registered in `lib.rs` beside
  `start_watchdog`.
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
  (`start_notify_poller` ticks every group regardless of its pause state).

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
  Resolution delivers `compact_reinjection_notice`, which embeds the pane's ACTUAL kickoff
  instructions file — read back verbatim from the durable file `write_instruction_files`
  already writes at spawn, not a pointer telling the agent to go re-read it (the issue's
  explicit preference: no reliance on the agent locating a file). This supersedes #287's
  optional immediate post-paste notice entirely — sending both would be redundant, and the
  immediate version risked landing while compaction was still running; the mandatory version
  only ever fires once compaction is actually observed to be done.
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
  here. Revisit if a real need for the pointer mode shows up.

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

**Per-CLI capability matrix (#416):**

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

### #417: compact hooks as a TRUSTED evidence source (Claude only)

Claude Code has a `PreCompact` hook (fires before manual AND auto compaction) and a
`SessionStart` hook with `matcher: "compact"` whose output can inject `additionalContext`
natively. **Copilot does not** — confirmed via its current changelog and an open, unresolved
upstream feature request explicitly asking for `preCompact` and stating its absence; Copilot's
own `sessionStart`/`sessionEnd`/`userPromptSubmitted`/`preToolUse`/`postToolUse`/`errorOccurred`
hooks exist but none fire specifically around a compaction. Per the #413 tier model, Copilot
stays on the pre-existing inference tier for this — not faked, not approximated with a
same-named event that means something else.

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

Nothing about the resolution logic itself — busy-then-quiet, the confirm gate, the
delivery-confirmation retry/abandon bounds, the per-agent arm-timeout — changed; the hook is
purely a new, stronger way to reach the SAME `compact_pending`/`compact_seen_busy`/
`compact_pending_trusted` triple every pre-existing arm site already writes.

**Verification limit, stated plainly:** whether `--settings`'s hook arrays ADD to a user's own
project/user `hooks.PreCompact` list or replace that event's list outright is not something this
change could verify empirically — the "never spawn a real agent CLI to test" constraint applies
here precisely because confirming it would mean actually running Claude Code through a real
compaction. The design leans on `--settings` being documented as an additive settings layer (the
same reasoning `--setting-sources`'s default-to-all-three implies); if that assumption turns out
wrong for hook arrays specifically, the user's first live demo from this feature's own worktree
is the fastest way to find out, and the fix (an explicit merge step reading the user's own
settings file) is scoped and known if needed. **If it turns out to REPLACE rather than merge**,
the consequence is not cosmetic: any of the user's own hooks for the SAME event (say, a
`PreToolUse` denial they rely on) would silently stop firing the moment loomux's own `--settings`
file is added to the command line — worth checking for explicitly on that first live demo, not
assumed benign.

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
- **Test-infra fix found along the way (not reviewer-flagged, surfaced by chasing an intermittent
  local flake):** `compact_hook_dir()` derives from `self.root.parent()`, which for
  `test_registry()`'s disposable tempdir is the SHARED SYSTEM TEMP DIRECTORY — so every test that
  spawns a Claude agent (the common case across the whole suite) was writing/reading the SAME real
  script file, racing every other such test. Fixed with a test-only override
  (`compact_hook_dir_override`/`set_compact_hook_dir_override`, mirroring the existing
  `copilot_agents_dir_override`/`claude_projects_dir` pattern), wired into all four `test_registry`
  helpers. Confirmed via 5 consecutive full-suite runs with no failures, after having reproduced
  the flake reliably enough to trace it.

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
  `merge_grants/pr-<N>` (a default-branch merge of PR N) or `release_grants/<tag>` (a
  release/tag publish). Line 1 is a unix-seconds **expiry** (`GRANT_TTL_SECS` = 30 min); the
  shim treats the grant as valid iff the file exists and now < expiry. Files are written with
  `atomic_write` (temp + rename, temp name = pid + `GRANT_SEQ`, no getrandom) so the shim can
  never read a half-written grant.
- **Claim, then settle on the real outcome (#256/#303).** A grant is not spent on
  interception — it is **claimed** (`loomux_grant_claim`: an atomic `mv` to a `.claimed`
  sibling, so a concurrent claimant loses the race outright rather than double-spending) and
  only **settled** (`loomux_grant_settle`) once the real `gh` call it authorizes has actually
  run: consumed (`rm`) on exit 0, restored to the original path on any other exit so a retry
  can still use it. A merge or release/tag publish GitHub itself refuses (draft PR, branch
  protection, a stale head, a transient API error, a tag that already exists) must not burn
  the human's one-time grant — live incidents for both the merge grant (#256, PR #226) and the
  release grant (#303) hinged on exactly this. If the shim process dies between claim and
  settle, the original grant file stays gone and the orphaned `.claimed` file is never
  consulted again — a crash requires a fresh grant, never a second use. Both gates share this
  one mechanism (not two copies).
- **Decision.** `gh_gate_decision` gains a `grant_valid` input: a default-branch merge is
  allowed by `(autonomous && auto_merge)` **OR** a valid grant for *that* PR (`AllowGrant`,
  consumed). The shim resolves the PR **number** via the real gh (`--json baseRefName,number`)
  so a grant for #5 can't authorize merging #7 whatever selector form was used.
- **Approve-with-comment.** The grant-writing methods (`grant_merge` / `grant_release`) take an
  optional comment delivered to the orchestrator with the authorization via
  `deliver_to_orchestrator` — "approved — also bump the changelog first". Board **Approve**
  (`approve_task`) now writes the merge grant for the task's PR and delivers the comment.
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
  (stdin body, opaque graphql, `DELETE …/releases/<id>` by numeric id), only the blanket markers
  (`autonomous && auto_release`, or supervised `dangerous && !autonomous`) can allow it — otherwise
  **fail-safe block**. A non-release api call (an issues endpoint, a branch `refs/heads` write, an
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
- **`last_user_input_ms` is untouched.** Every human write still stamps it (the quiet backstop,
  attention routing, and the stranded-flush guard all rely on it); `input_pending` is a separate,
  additive flag written under the same `ptys` lock so the pair can't tear.
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

- *Ordering is best-effort, not a strict FIFO guarantee.* The correctness property is
  atomicity — each message lands whole. Order is **not** guaranteed under rapid concurrent
  sends: `deliver_prompt` spawns a thread per delivery that contends for the per-pty `delivery`
  `std::sync::Mutex`, which is not fair/FIFO (SRWLOCK on Windows), so two sub-second sends — or a
  steer racing a report — can acquire the lock out of submission order. Nothing is lost or
  corrupted (mutual exclusion still holds); only the relative order of near-simultaneous
  messages may flip. A strict arrival sequence would mean threading a monotonic seq/queue
  through the shared `deliver_prompt` hot path (used by *every* delivery source — kickoffs,
  reports, watchdog nudges, steer); not worth it for a low-impact reorder window the human can
  avoid by letting one message land (visible in the pane) before sending a dependent correction.

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
    its CLI is launched **read-only** (`build_agent_command(read_only=true)`): on Claude
    `--disallowedTools Edit Write MultiEdit NotebookEdit` plus `Bash(git commit *)` /
    `Bash(git push *)`, on Copilot `--deny-tool write|edit` plus `shell(git commit|push)`
    — deny rules override the allow list / Auto perms on both CLIs. So a planner **cannot
    edit files, commit, or push**, i.e. cannot produce code changes or push a branch.
    (Rule-spelling note: on Claude the `:*` wildcard is valid *only* as a trailing suffix.
    An earlier draft also passed the colon-mid forms `Bash(git commit:*)` / `Bash(git push:*)`
    as redundant spellings; those are **malformed** — Claude Code ignores them *and* prints a
    startup warning, which was the "auto deny rule" flash seen on planner boot. The canonical
    space form is the only spelling now emitted; see the plan-mode decision below.)
  - *Instruction-backed* (the template + kickoff `PLANNER_READONLY_NOTE`, not a sandbox):
    `gh` stays allowed (a planner needs `gh issue comment` for its deliverable), so a
    planner *could* technically run `gh pr create` or create an inert local branch — it is
    told not to, and with commit/push denied such a branch carries nothing. This is a
    deliberate trade (plan-comment-as-deliverable over a full jail), now stated honestly
    rather than presented as an absolute guarantee.

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
  - **So the planner keeps Auto + structural deny rules** — which is the *autonomous*
    equivalent of plan mode's intent: read-only research, but free to emit its plan and
    report and then exit without waiting on anyone. To make that hold with **no human in the
    pane**, a `read_only` planner is now launched **unattended regardless of the group's
    `auto_ops`** (`unattended = auto_ops || read_only` in `build_agent_command`, applied to
    **both** CLIs): on Claude, Auto perms + a pre-approved `Bash(git *)` / `Bash(gh *)`
    allowlist; on Copilot, `--autopilot --allow-all-tools --allow-all-paths` — so
    exploration, `gh issue view`, and the `gh issue comment` plan never prompt, with edits +
    `git commit`/`git push` denied on both (deny takes precedence over Auto / `--allow-all-tools`).

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
