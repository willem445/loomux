# Polled views: the published snapshot the group view and tab strip read

Status: implemented (#1625, not yet on main). Issues: #1608 (this slice), #1600 (the epic and its
plan §3 Phase 1), #1604 review N3 (the staleness requirement, deferred here so
the app has one staleness mechanism rather than two), #1595 and #1592 (the two
releases whose remedies this replaces).

Two frontend loops used to read the orchestration registry directly:

- `groupview.ts` `load()` — every 2 s per open group panel, a `Promise.all`
  batch of **ten** `orch_*` commands;
- `tabbar.ts` `pollStatusOnce()` — every 4 s, `orch_group_summary` +
  `orch_group_usage` for **every group-bound tab**, awaited in turn.

Every one of those acquires an `OrchRegistry` mutex, and `lock_safe` was an
infallible acquire — no timeout, no try-lock, no bound. So one long hold
anywhere in the registry parked every poller for as long as it lasted. (#1609
added a bounded form; see `doc/design/lock-liveness.md`. It does not make this
section stale: a bounded poller would still be a poller waiting, and Phase 1
removes the acquisition rather than bounding it.)

They now make **one** call each, and neither takes a registry lock at all.

## Why "make them async" was not the fix

It was the previous fix, twice, and the second one is the reason this note
exists.

#1592 converted an expensive polled command; #1595 converted five cheap ones,
having established that `cheap` bounds a critical *section* and not an
*acquisition*. Both were correct about the thread they were clearing, and both
moved the same unbounded wait onto a different victim. Post-#1595 a polled
command parks a **blocking-pool** thread instead of the webview thread, and
nothing in the tree configures `max_blocking_threads`, so that pool is tokio's
default 512 — shared with `write_pty`, the app's input path. At the 2.5-5
parked threads per second those two loops produce, 512 is reached in minutes,
and from then on no pane accepts input while the window keeps painting. That is
the beta6 field report, and #1600 §1.2 derives it from the code.

#1602/#1604 made the *accumulation* unreachable by single-flighting both poll
sites — one outstanding call per site, never a queue — and that gate stays. What
it could not do is make the surface recover: a call that never settles leaves
the panel rendering its last payload with no disclosure at all (#1604 review
N3).

**The polled reads never needed the live registry. They needed a view of it.**

## The mechanism

`crates/loomux-engine/src/published.rs` — `Published<T>`, an
`RwLock<Arc<Stamped<T>>>`. `load()` is a read-lock, a pointer clone and a
release; `store()` is a write-lock and a pointer swap. The writer's critical
section holds no IO, no other lock, and nothing that can park, so the longest a
reader can wait is that swap — bounded by construction rather than by what the
producer happens to be doing. That is the property an `ArcSwap` dependency would
have bought, and the reason none is added (CLAUDE.md constraint 2 makes every
new crate in `src-tauri`'s graph a question worth not asking).

Two details in `store` are load-bearing and are pinned by that module's tests:
the **previous** snapshot is dropped *outside* the guard (freeing a deep
`serde_json::Value` is real work, and doing it under the lock would put
unbounded work back into the one section this promises is bounded), and `seq` is
derived *under* the lock rather than from a separate counter, so a sequence
number can never label a different value.

`src-tauri/src/orchestration/views.rs` — `ViewPublisher` owns one thread. Every
`VIEW_PUBLISH_INTERVAL` (1000 ms, equal to `USAGE_POLL_MAX_AGE`, so a payload
served to a 2 s poll is never staler than the usage memo that poll reads today)
it takes the group id list out from under the `groups` mutex, **releases it**,
and computes each group's payloads. Nothing holds a cross-group lock across a
compute (INV-5).

Every section is produced by the **same** registry function the command it
replaces calls — `group_summary`, `group_usage_live_within`, `is_paused`,
`notify_enabled`, `spawn_expanded`, `autonomy_state_within`, `group_watches`,
`workflow_status`, `mergeqview::merge_queue_view`, `lock_state` — so the wire
shapes are identical by construction rather than by a second implementation that
has to be kept in step. `a_published_view_carries_every_payload_the_ten_commands_return`
(`src-tauri/tests/views.rs`) pins it, comparing each section against a direct
call after blanking two clock-derived keys it names and counts.

The ten commands stay. `tasksview.ts` reads summary and workflow status when it
opens, and a once-per-open read is not what this phase is about.

## The two tiers, and the lease

| tier | sections | computed for |
|---|---|---|
| strip | `summary`, `usage` | every group in the registry, plus every group holding a **strip lease**, every tick |
| view | the other eight | only a group holding a **view lease** |

**Both tiers are lease-driven, and the strip's lease is not an optimisation —
it is a correctness requirement.** The registry's group map holds what this
session created or resumed. A tab can be bound to a *restored* orchestration,
which lives on disk and never enters that map (`list_recorded` reads the root
directory directly). Publishing only registry-known groups therefore dropped
those tabs' status entirely, losing the accrued-cost badge #194 P4 LOW-8 put
there on purpose — a regression against the per-tab reads this replaced, which
answered for **any** id the caller named. Found by the E2E soak lane's binding
witness on its first run (#1625 review round 2).

So `orch_strip_view` takes the caller's bound ids and stamps a strip lease per
id, and the publisher covers `reg.groups` ∪ fresh strip leases. Still one IPC
per sweep. A leased id the registry does not know is computed through the SAME
functions as any other — `group_summary` answers zero live agents,
`group_usage_live_within` reads that group's `usage.json` — so its entry is
wire-identical to what the commands it replaced returned for it, by
construction rather than by a second disk path.

Two things were rejected. Reading `tabs.json` backend-side couples the
publisher to a UI persistence file. Publishing every on-disk group is unbounded
on a long-lived machine — which is what the lease's expiry-by-age exists to
prevent: a strip lease is released by AGE, never by registry membership, since
membership could not release a group that was never a member.

`orch_group_view` stamps `lease(group) = now`; the publisher computes the view
tier while that lease is younger than `VIEW_LEASE_MS` (10 s — five poll
periods, so a 2 s poll keeps its lease alive with four ticks of slack).

The lease exists because the view tier is the expensive half: a
`merge_queue.json` read, a `workflow.yml` parse, a memoised `git`
default-branch resolution and a resource reconcile. Without it, opening **one**
group view would put all of that on **every** group in the app, every second.
With it, the rate is exactly today's: one open view.

A lapsed lease **drops** the tier rather than carrying it forward. Carrying it
would let a panel reopened ten minutes later render ten-minute-old toggles under
a one-second `age_ms` — the false freshness this whole slice exists to remove.

## Freshness is per group, never per snapshot

`GroupView::computed_at` stamps each group individually, and every
`age_ms`/`stale` decision reads *that*, not the snapshot's own publication
stamp. A snapshot-wide stamp would be a lie in two ways that both really happen:
`publish_group_now` republishes with exactly **one** group recomputed, and a
group created between passes is younger than the map it arrives in.

`orch_strip_view`'s `meta` therefore reports the **oldest** group's age, which
means "nothing in this payload is older than this". An average, or the
snapshot's stamp, would report the strip as fresh because one tab happened to
move — and the strip's whole job is to be right about the tab that is in
trouble.

## The write-side nudge

The group view re-reads immediately after every toggle, not on the next tick, so
a published snapshot alone would show the toggle snapping back for up to a
publish interval. `OrchRegistry::publish_group_now(group)` recomputes that one
group synchronously, on the mutating command's own pool thread, after the write
returns and with no registry guard from it still held. It is `usage_memo`'s
"invalidate where being late is wrong" rule applied to writes rather than to a
memo.

It is appended to every mutation the group view re-reads after — the set is
derived by grepping `groupview.ts` for an `await this.load()` following an
invoke, not from memory. Nudging does not grant a view lease, so a mutation
arriving from the MCP side on a group nobody is looking at stays a strip-tier
recompute.

### Two publishers, one rule: the later stamp wins

There are two writers — the thread's full pass, and a mutating command's
single-group nudge — and each is a read-modify-write of the same cell. Without
mutual exclusion that is a lost update, and the losing case is the one that
matters: a pass stamps every group with the instant it STARTED, so a nudge that
lands while that pass is still computing carries a strictly later stamp, and a
pass stored wholesale would revert exactly the toggle the group view is about
to re-read.

So `publish_lock` serializes the swap (never the compute, which stays outside
it), and a pass MERGES per group rather than replacing the map: an existing
entry with a later `computed_at` is kept. That is monotone rather than a latch
— the next pass starts after the write and replaces the nudged entry with
genuinely fresher data. `a_nudge_is_not_lost_to_a_pass_that_was_already_computing`
pins both halves, including that a later pass does win.

**No reader takes `publish_lock`.** `load()` goes to the `Published` cell
directly, so nothing about the liveness property depends on it, and its own
critical section is a map clone plus a pointer swap — the map holds an `Arc`
per group, so a single-group republish clones pointers rather than ten
`serde_json::Value` trees per untouched group.

### The publisher thread is supervised (#1702 — this was the disclosure)

`start_view_publisher` used to spawn a bare `loop` with no `catch_unwind` and no
restart, like every sibling `start_*` loop in `mod.rs`. A panic inside one
group's `compute_group` ended it **permanently** and froze the snapshot for
**both** UI surfaces at once — this thread is load-bearing for two whole
surfaces, where the other loops each drive one badge. The badge was the only
thing between a dead publisher and a permanently-frozen, entirely plausible UI,
which is why it was written down here rather than left to be noticed.

#1702 closed it, for the reason that made it urgent: a re-entrant `lock_safe` is
now REFUSED by unwinding rather than parking (`doc/design/lock-order.md` §2.1),
so a class of defect that used to wedge this thread now panics it, and the
degraded result had to be well defined before that was an improvement. Each pass
runs under `obs::TickSupervisor`: a panic costs **one pass** and a
`tick-panicked tick=view-publisher` breadcrumb, and the next tick republishes.

The disclosure that remains is narrower, and it is the supervisor's own bound
rather than this design's. A body that panics on `TICK_PANIC_LIMIT` consecutive
passes is latched off — because supervision otherwise turns a deterministic
panic into one crash log every second, and nothing prunes those — and a
`tick-disabled tick=view-publisher` breadcrumb says so. At that point the
snapshot freezes exactly as it used to, and the stale badge is again the only
thing showing it: same failure, reached after three tries instead of one, and
named in the log this time.

Either way it degrades the way the rest of this design does: `age_ms` grows, the
badge appears, and the panel says it is stale rather than looking live.

## The wire contract

```
orch_group_view(group_id) ->
{ "meta": { "seq": u64, "published_at_ms": u64, "age_ms": u64, "compute_ms": u32,
            "stale": bool, "partial": bool, "view_ready": bool },
  "summary": <orch_group_summary payload>, "usage": <orch_group_usage payload>,
  "paused": bool|null, "notify": bool|null, "spawn_expanded": bool|null,
  "autonomy": <orch_autonomy payload>|null, "watches": <orch_group_watches payload>|null,
  "workflow": <orch_workflow_status payload>|null,
  "merge_queue": <orch_merge_queue payload>|null,
  "locks": <orch_lock_state payload>|null }

orch_strip_view() ->
{ "meta": { ... as above, without view_ready ... },
  "groups": { "<group id>": { "summary": ..., "usage": ... } } }
```

`published_at_ms` is a wall clock, carried for a human to read and **never**
what a staleness decision is made from: a wall clock moves backwards (NTP, a VM
resume, a manual set) and a rule built on one reports a snapshot from the
future. `age_ms` is measured backend-side from a monotonic `Instant` at the
moment the read is served.

**`view_ready` is `orch_group_view`'s only addition to the shape**, and it says
whether the eight view-tier sections are present. They are absent together or
present together — a type-level fact (`Option<GroupViewTier>`), not a flag
someone has to remember to set — and when absent they are `null`, never
defaulted: a fabricated `paused: false` is a wrong answer rendered as a right
one. `view_ready` is `false` on the first read after a panel opens (the lease
stamp and the next publish pass crossed) and after a lease lapsed while the
panel was closed. The caller keeps its previous render and re-asks on a bounded
ladder — `VIEW_TIER_RETRY_MS` (250 ms), at most `MAX_VIEW_TIER_RETRIES` times,
with the budget released by **evidence** (a tier that arrived), never by elapsed
time.

**A refused or unknown group id answers `Value::Null`** — the same
no-error-channel degrade the ten commands use (`command_group`'s doc). A group
created since the last publish pass answers `Null` too, deliberately: the
caller's response to both is identical — keep the previous render, ask again.

## Staleness (INV-6)

`meta.stale = age_ms > VIEW_STALE_AFTER_MS` (5000 ms), decided **backend-side**,
so the app has one definition of "stuck" rather than two that drift. It is
entered on the clock and released **only on evidence**: nothing takes the badge
down but the next successful `store`, which is what re-stamps `computed_at`. A
badge cleared by a frontend timer would come down while the registry was still
wedged, which is exactly the "release on independent evidence, not elapsed time"
rule `.orrerix/lessons.md` states.

If the registry wedges, **exactly one thread parks** — the publisher's — and
every reader keeps answering with the last snapshot and a growing age. Bounded,
visible, recoverable.

`src/viewstale.ts` is the frontend half: DOM-free, unit-tested, and deliberately
unable to overrule the backend's `stale` from `age_ms` (two clocks and one
threshold is how the two halves come apart). It renders on **both** surfaces —
the group view's header badge and the tab strip's status chips — because both
froze identically before this, and `src/singleflight.ts`'s own header used to
disclose the trade for the strip alone.

`meta.partial` is reserved for plan Phase 2.1, where a section that hits a
`Busy` timeout keeps its previous value and flips it. It is always `false` here:
Phase 1 has no bounded acquisition to time out, so no section can be partial. It
is in the contract now so the renderer does not change shape when 2.1 lands, and
`viewstale.ts` already gives it a distinct label — a payload where *some*
sections are current must not claim the whole panel is frozen.

## What this deliberately does not do

**Push instead of poll.** Removing the poll IPC entirely is the obvious next
step, and it is the next step rather than this one: every hidden-window and
closed-panel visibility rule in the app (INV-3/INV-4, `pollgate.ts`,
`wakegate.ts`) is written for pulls, #1604's single-flight is a pull fix, and a
push per group per second is one webview `eval` per event
(`performance.md` §1 — the cost is per event, not per byte).
`queue_depth_push` (`orch-queue-depth`) is the precedent to copy when it
happens, specifically its compare-before-emit shape.

**Remove `usage_memo`.** It becomes redundant the day the publisher is the only
`group_usage_within` caller. The `group_usage` MCP tool still reads it, so it
stays until that tool reads the snapshot too — a follow-up, not a silent
widening of this slice.

**Bound the acquisition.** The publisher thread itself still waits on
`lock_safe`, unbounded. That is plan Phase 2.1's job (`lock_timeout`, a typed
`Busy`, and `partial` above); what Phase 1 changes is that exactly one thread
pays that wait instead of every poller, and that the surface says so.

## What this changed in the E2E soak lane (#1603/#1606)

Two of that lane's assertions were about the poll path this phase rewrote, so both had
to move with it. Recorded here as well as in the spec, because a reader arriving from
either side should find the same story.

**A held registry lock no longer makes the strip's sweep skip.** #1606 asserted
`skipped >= 1`: `orch_group_summary` took `groups`, so a hold parked the sweep and
#1604's single-flight gate declined the next tick. The sweep now reads a published
snapshot and takes no registry lock, so it settles under a hold — the assertion is
inverted to `skipped === 0`, plus the disclosure half this phase owes: under the hold the
strip keeps answering **and** reports `stale`, clearing on the next successful publish.

Stated as a loss too: `skipped >= 1` has no witness left in that lane and cannot have
one, because the condition that produced a skip is the condition this phase removed. The
gate itself is still covered directly by `test/singleflight.test.ts`.

**The binding witness had to be replaced.** #1606 read "the corpus really bound its
tabs" out of the poll's IPC fan-out — two invokes per group-bound tab, so the dispatch
count scaled with the tab count. Collapsing the sweep to one `orch_strip_view` deletes
that scaling, so the witness was gone structurally rather than stale.
`__tabStatusStats()` (`src/tabbar.ts`) replaces it, naming the group ids the strip
resolved and which of those the snapshot carried — and it reports on the strip's FAILURE
path too, so a strip whose reads are throwing cannot masquerade as recovered.

## Enforcement

`src-tauri/tests/perf_dispatch.rs`'s poll-path guard (E1, test **L6** in the
plan's table) reads the two poll sites out of `groupview.ts` and `tabbar.ts`,
resolves each call through `orchestration.ts`'s wrapper map, and asserts three
things: every command reached is async (the #1595 half, kept — and by itself it
would have passed on beta6, which §2.2 says in as many words), every command
reached is in `SNAPSHOT_SERVED`, and each of those bodies contains
`views.load(`. The set of commands reached must *equal* `SNAPSHOT_SERVED`, so a
poll site that quietly acquires an eleventh call and a manifest row that stops
being reached both fail.

Its stated bound: comments are stripped before brace matching, so a `{` in prose
cannot end a poll body early; string and template literals are not, and the
balance assertion is what turns that into a loud failure rather than a silently
short body. Neither poll body contains one today.

`src-tauri/tests/views.rs` pins what the publisher produces (wire identity, the
tier split, the lease lapse, the nudge, the staleness edges, the null degrades);
`src-tauri/tests/liveness.rs` L1 pins the property this is all for — that both
reads return while **every** tracked registry lock is held, and that the stale
flag flips on the clock and clears on a publish.
