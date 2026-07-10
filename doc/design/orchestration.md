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
| `spawn_agent(name, kind, task, worktree?, branch?)` | ✓ (guardrailed) | ✗ |
| `send_prompt(agent_id, text)` | ✓ | ✗ |
| `report(status, summary)` / `message_orchestrator(text)` | ✗ | ✓ |
| `list_agents()` | ✓ | ✓ |
| `get_output(agent_id, lines)` | ✓ | ✗ |
| `kill_agent(agent_id)` / `focus_agent(agent_id)` | ✓ | ✗ |
| `rename_agent(agent_id, name)` | ✓ | ✗ |
| `get_state()` | ✓ | ✓ |
| `set_state(state)` | ✓ | ✗ |
| `group_usage()` | ✓ | ✗ |

Guardrails enforced by `spawn_agent`: live-agent cap (`max_agents`, counting workers +
reviewers + planners), CLI + model pinned per role (`{role}_cli` / `{role}_model`, see
**Plan agent + mixed agent types** below), permission mode fixed at group creation
(`acceptEdits` default; full-auto opt-in). Worktree creation reuses `git_worktree_add`
(never for a planner — it is read-only).

`kind` is `worker` (default), `reviewer`, or `planner`. A **planner** explores the
codebase read-only and writes a structured implementation plan as a GitHub issue comment,
then reports and exits; it never writes code, branches, worktrees, or PRs.

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
monitor → reviewer per PR → findings addressed → high-level completion check → hand to
user for merge. Workers: branch → implement → meaningful unit/functional tests (test
intent, not vacuous passes) → design notes + user docs → commit → push → `gh pr create`
→ report. Reviewers: `gh pr review` with findings → report.

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
  review → pr → human-testing → done, plus blocked; notes; priority order), edited by
  the orchestrator via MCP tools and by the human via the pane overlay (Alt+T); each
  side's edits notify the other, and everything is audited.
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
  shim treats the grant as valid iff the file exists and now < expiry, and **consumes it**
  (`rm`) on use — one action only. Files are written with `atomic_write` (temp + rename, temp
  name = pid + `GRANT_SEQ`, no getrandom) so the shim can never read a half-written grant.
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
  `view`/`list`/`download` pass through) — `gh_release_action`.
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
  bare `v*` tag push under `-C otherdir` the confirm can't see; (3) run in a **cmd/pwsh** pane
  with no `sh` on `PATH` (the `.cmd` fallback runs the real binary); (4) define a **`gh`/`git`
  alias** re-invoking the real binary; (5) **forge a grant file** directly (`echo … >
  $LOOMUX_GROUP_DIR/merge_grants/pr-N`) — the grant dir is on disk under the agent's own uid, so
  its *existence* isn't cryptographically unforgeable; the "human-only" boundary is that no
  loomux surface (MCP) mints one, not that the filesystem forbids it. All the same class as
  absolute-path gh. Catching every shape needs sandboxing or network egress control, out of
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
      (`COPILOT_GROUP_AUTOPILOT_FLAGS`). The **single-pane** posture stays
      `--allow-all-tools --allow-all-paths` (`COPILOT_UNATTENDED_FLAGS`, no `--autopilot`): a
      human is at that pane, interactive framing is correct, and no one wants an unbidden
      startup dialog. The two atoms are pinned to differ only by `--autopilot` (a test asserts
      `GROUP == "--autopilot " + single`).

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
      The confirm is gated to a **fresh boot** (the `Delivery::FreshKickoff` classification →
      `should_confirm_copilot_autopilot`): a **resume** restores allow-all/autopilot from
      Copilot's session event log so no dialog reappears, and mid-session follow-ups/steers are
      long past boot — both skip the watch rather than eat its fail-soft wait on every delivery.
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
