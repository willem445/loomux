# Design: named lock resources (#858)

Status: implemented (PR #859). Config in `orchestration::workflow`, engine in
`orchestration::locks`, wiring and lifecycle in `orchestration::mod`, tools in
`orchestration::mcp`, chrome in `src/locklines.ts` + `src/groupview.ts`.

## Problem

Several agents in one group share one machine. Some of what they reach for is singular:
a compile that saturates every core, a GPU, a device on a port, a fixed port number, a
staging database. Nothing in loomux ever told them to take turns, so four workers started
four builds and the machine fell over.

The immediate motivation was concrete: during an Actions-budget pause, local compile/test
work had to be serialized, and it was done with a **hand-rolled `mkdir` lock written into
worker briefs**. That worked, and it was not a mechanism — it was prose, per brief,
unaudited, with no reclaim if the holder died. This issue makes it first-class.

## What this is NOT: the enforcement design, and why it was abandoned

#318 asked the same question and got a different answer: shadow the guarded program on
`PATH` (the `gh`/`git` shim pattern), count slots in a semaphore directory, and make
`cargo build` itself wait. #322 implemented it and was **closed unmerged** after live
testing (#335): the shim only intercepts invocations that go through the shell it shadows.
A PowerShell or absolute-path invocation walked straight past the `.cmd` delegator, so the
guarantee the design appeared to give was not one it could keep. Covering every invocation
shape reliably is a much larger surface than the problem justifies.

So this design does not attempt enforcement at all, and says so in every place an agent or
a human reads about it. **An agent holds a lock because it asked for one.** What loomux
supplies instead is the part a hand-rolled lock cannot: a declared vocabulary, a fair queue,
bounded holds, automatic reclaim, and an audit trail of who held what for how long.

The honest framing matters more than it looks. An advisory lock that is *described* as
advisory is a tool a human can reason about ("w-3 has held `build` for 40 minutes — is it
stuck?"). An advisory lock described as enforcement is a claim the code does not back, which
this repo treats as a defect in its own right.

## Config: `resources:` in `.loomux/workflow.yml`

```yaml
resources:
  build: { slots: 1, max_hold_minutes: 45 }
  gpu:   { slots: 2 }
```

Two numbers, keyed by a name the repo chooses. That is the whole schema, and it is the whole
of CLAUDE.md constraint 8's requirement: **loomux never learns what `build` means.** There is
no `program:`, no `command:`, no `match:` — nothing that would make the product code know
about a toolchain. Compare #322's design, which had to carry `program`/`args` matchers
precisely *because* it was trying to intercept commands; dropping enforcement drops the
toolchain knowledge with it. That is a second, unplanned argument for the cooperative shape.

**Restrict-only.** `workflow.yml` is untrusted repo input (whoever opens a PR authors it), so
every clause is measured against the capability-closure spine in `workflow.rs`'s module doc.
`resources:` can only ever make an agent *wait*. It names no branch, no reviewer, no program
and no agent; nothing in the merge or release path reads it; and `deny_unknown_fields` on
`RawResource` makes a gate-shaped key a hard parse error rather than an ignored line. The
`RawWorkflow` field-inventory test in `workflow.rs` forces a human to make that argument
again for any field added here. The residual abuse is a hostile PR declaring `slots: 1` on
something everyone needs, to slow the group down — bounded by `max_hold_minutes`, visible in
the panel, and audited throughout.

**Every bad value is a hard error, never a substitution.** `slots: 0`, `slots: 65`,
`max_hold_minutes: 0`, `max_hold_minutes: 481`, a name outside the identifier alphabet, more
than `RESOURCES_MAX` resources — all fail the parse of the whole file. This is
`merge_queue.max_batch`'s posture, for its reason: a repo that wrote `slots: 0` believes its
builds are serialized, and silently substituting the default would leave that belief in place
while the behaviour changed underneath it. Names are *rejected*, never rewritten, for the
`blocks[].id` reason: an author who wrote `heavy build` must not end up with a resource called
`heavybuild` that the `acquire_lock` call in their own worker brief cannot name.

**Gated on the advanced orchestrator**, like every other clause in this file: with the toggle
off, `workflow.yml` is not the group's config and is never opened. One reader
(`OrchRegistry::lock_resources`) for the whole block, the rule `merge_queue_policy` states —
a tool and a background sweep that disagreed about the declared set would grant and reclaim
on different worlds.

**Empty means off, all the way up to the tool listing.** `mcp::tool_defs` takes the declared
menu and omits all three tools when it is empty, so a repo that declares nothing pays neither
the behaviour nor the context. The declared names are folded *into* the `acquire_lock`
description, because an agent that cannot see what exists guesses (`cargo`, `build-lock`,
`ci`) and collects refusals instead of a lock.

## The load-bearing decision: acquire never blocks

`acquire_lock` returns immediately — granted, or queued with a position — and a grant that
arrives later is **typed into the caller's pane** as a `[loomux]` notice, exactly like a
`notify_when` watch resolving.

The alternative (block the MCP call until the lock is free) is a deadlock by construction, and
loomux has already paid for that lesson once. A `[loomux]` notice is delivered by *writing into
a pane*, and a pane that is mid-call cannot take a delivery — so an agent blocked waiting for
its lock would be blocking the delivery of the message telling it the lock is ready. That is
#590's shape reached by a new route, and it would be worse than #590 because nothing external
resolves it: the queue only moves when loomux writes to the pane.

This is why the queued reply is worded the way it is. It states the position, and then it says
**END YOUR TURN** and explains what will wake the agent — because a worker told only "queued"
reliably invents a sleep loop, and the sleep loop is the deadlock.

## Semantics, and the reasoning behind each

- **Slots.** `holders.len() < slots` admits; otherwise the request joins the queue. `slots: 1`
  is a mutex; anything more is a counting semaphore.
- **FIFO.** `VecDeque`, push back, pop front. #322's `flock`-poll design could not offer this
  and documented starvation as a known caveat bounded by a timeout; a cooperative queue held in
  one process gets fairness for free, so it takes it.
- **Idempotent re-acquire, with no refresh.** Asking again while holding reports the existing
  hold and its **original** deadline; asking again while queued reports the **original**
  position. Both matter after a `/compact`, when an agent cannot remember whether its own call
  landed — "just ask again" has to be safe. The no-refresh half is the important one: a hold
  whose deadline moved every time its holder said "still mine" would be an unbounded hold with
  extra steps, and `max_hold_minutes` would bound nothing.
- **Release of a lock you do not hold is an error** — you believe you are serialized and you
  are not, which is worth being told. **Release while queued withdraws the request** and says
  so. That third state is deliberate: without it a worker that no longer needs the resource
  cannot leave the queue except by timing out, and would then be *granted* a slot it never uses
  and sit on it for a full `max_hold_minutes` while everyone behind it waits.
- **Live config reload.** Every entry point reconciles the table against the file
  (`LockTable::sync`): a new resource appears, an undeclared one is removed (and what it took
  with it is audited — dropping live holders silently would be the claim-the-code-doesn't-back
  defect), and `slots`/`max_hold_minutes` retune in place. A **shrunk `slots` never revokes a
  live hold**; it stops new grants until the count falls back under the new limit. That is the
  only reading that cannot yank a resource out from under a build already running, and existing
  holds keep the deadline they were granted under for the same reason.

## Every wait and every hold is bounded

The lessons-file rule — a suppression driven by a fallible signal must be bounded — applies
twice here, because "the holder will call `release_lock`" and "the waiter still wants this"
are both fallible.

| bound | ends how | who is told |
| --- | --- | --- |
| `max_hold_minutes` | sweep reclaims the hold (`lock-expired`) | the ex-holder, in its pane: your work is no longer serialized |
| `wait_minutes` (5–240, default 60) | sweep drops the request (`lock-wait-timeout`) | the waiter, in its pane |
| holder's pane dies | `mark_dead` reclaims immediately (`lock-reclaim`) | nobody — there is no pane; the next in line is told it has the lock |
| waiter's pane dies | `mark_dead` drops it (`lock-wait-cleanup`) | nobody |

`wait_minutes` is clamped by `notify::clamp_expires_minutes` — the same function, not a second
copy of the same bounds, because it is the same quantity: a bounded wait on something external.

**Two reclaim paths on purpose.** `mark_dead` is the fast path (every kill, crash, idle-reap and
planner auto-close funnels through it, beside `cleanup_agent_watches`), so a finished worker's
slot is free instantly rather than up to 30s later. The sweep is the backstop for a holder that
somehow leaves without that path running, and it is the *only* path for the two clock-driven
cases. Neither alone is sufficient: the fast path cannot see an overrun, and the sweep alone
would make every hand-off wait for a tick.

**The sweep lives in `gh_poll_tick`**, beside `notify_tick`, rather than owning a thread: same
30s cadence, same paused-group freeze, and no I/O of its own beyond the audit lines it writes.
It takes `now` as a parameter, which is what makes expiry testable without waiting 45 minutes.

**A paused group is frozen solid** — no expiry, no reclaim, no hand-off — and is credited the
whole pause span on the tick that observes it unpaused. This is `notify_tick`'s TTL-freeze,
applied to locks, and it needs its own `paused_locks_since` map rather than sharing the watches'
one: two tick paths crediting the same map would charge one pause span twice. A pause is not a
reason to take a running build's lock away.

## Persistence: none, deliberately

Lock state is `Mutex<HashMap<group, LockTable>>` on the registry, in memory only — the same
lifetime class as `watches` and `channels`. A lock file surviving a restart could only ever
describe holders that no longer exist, because every pane that could have held a lock died with
the process. So there is nothing to reconcile on startup and no stale-lock-file failure mode,
which is a strictly simpler system than the durable one and loses nothing real.

## Layering

The state machine (`locks.rs`) is a pure function of (state, clock, liveness): `sweep` takes
`now` and an `is_live` closure, and knows nothing about the registry, panes, or the audit log.
That is what lets its contention, FIFO, reclaim and idempotence properties be unit-tested
exhaustively with a fake clock. The registry half is deliberately thin — reconcile, call, audit,
deliver — and is covered by integration tests driving the real `dispatch()`, because the parts
that can silently do nothing (the listing, the role gate, the pane notice, the `mark_dead` hook,
the sweep's place in the tick) all live there rather than in the engine.

No registry mutex is ever held across `audit` or `deliver_prompt`: both block (file I/O, a
per-pane delivery mutex). `with_locks` returns what the reconcile dropped and audits it after
releasing the table, and `locks_tick` snapshots liveness from `agents` *before* taking `locks`,
so those two are only ever acquired in one order.

## Visibility

One payload (`OrchRegistry::lock_state`) serves both `list_locks` and the `orch_lock_state`
Tauri command, so what the human sees beside the panes and what an agent reads can never
disagree. The chrome is a row in the group lifecycle overlay — chrome inside an overlay that
already floats over the terminal, so CLAUDE.md constraint 1 is untouched, and `minChromeHeight()`
measures it rather than being told about it. Formatting lives in the DOM-free `src/locklines.ts`
so it is unit-testable; the tone ladder (neutral → amber when somebody is waiting → red when a
reclaim is imminent) reuses the panel's existing colour vocabulary rather than inventing one.

## Known limits, stated rather than hidden

- **Bypassable by construction.** An agent that never calls `acquire_lock` is not serialized.
  This is the design, not a gap in it — see the enforcement section above.
- **Per group, per process.** Two groups on the same machine contending for the same physical
  resource do not see each other. A machine-wide scope is possible (root the table outside the
  group) and is deliberately not built: nothing has asked for it, and the per-group scope is the
  one the queue's own fairness and audit trail are meaningful in.
- **A granted lock can idle.** An agent granted a lock while mid-turn will not read the notice
  until its turn ends. `max_hold_minutes` bounds that, and it is the same bound that covers a
  holder that simply forgets.
