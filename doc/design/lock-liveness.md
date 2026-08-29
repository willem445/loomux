# Bounded lock acquisition — budgets, `Busy`, and why the unwind is safe

Phase 2.1 of the responsiveness-root-cause epic (#1600, issue #1609). Builds on
Phase 0's instrumented locks (#1601) and Phase 1's snapshot publisher (#1608).

Two things live here. The **mechanism** — a thread-local budget that bounds
every tracked-lock acquisition underneath it — and the **contracts** it
publishes: the two shapes an MCP caller can now receive instead of silence, and
the "partial" a polled view can now show instead of a stale number nobody
labelled. Both are public: an agent's model reads the first, a human reads the
second, and neither can be changed without changing what they mean.

§4 is rider R1 and is the section a reviewer should hold hardest: it is the
argument that abandoning a read path partway through cannot corrupt anything.

---

## 1. What was unbounded, and what a bound buys

`obs::LockExt::lock_safe` — and `TrackedMutex::lock_safe` after Phase 0's type
swap — is `Mutex::lock` with poison recovery. It has no timeout and no try-lock,
so **a caller's cost is set by a lock's worst holder, not by its own body.**
That is `performance.md` §1's resource 4, and it is the one every remedy of the
last three betas moved a victim of without removing.

The epic's §1.2 gives the chain: one wedged registry mutex, then every MCP
request thread parked in `resolve_token` *before dispatch*, then the shared
blocking pool filling with polled reads, then every pane refusing input. Phase
2.3 took the input path out of that pool (#1607) and Phase 1 took the poll path
out of the registry entirely (#1608). What neither could reach is the MCP half,
because it fails before any of their machinery runs.

That last hole is **measured, not inferred.** The E2E soak lane (#1606) holds
`groups` for 90 s and probes. A keystroke lands. An MCP `ping` — which answers in
6 ms normally and takes no registry lock of its own — gets **no answer in 20 s**
(`mcp ok=false in 20004ms`; runs 33234732464, 33238467220, 33240132717). The
reason is one line in `OrchRegistry::resolve_token`:

```rust
// Dropped the `agents` lock before taking `groups` — resolving the
// spawning block's role_hint needs both, and locking them together
// would pin a lock order no other call site promises to respect.
let role_hint = self.group(&a.group)
    .and_then(|g| g.guardrails.block(&a.block).and_then(|b| b.role_hint.clone()));
```

`OrchRegistry::group` takes `groups`. Every request pays it, `ping` included, and
no amount of snapshot-serving reaches a request that never gets past its token.

A bound does not make the registry available. It converts **an unbounded wait
into a bounded one plus a truthful answer** — which is the difference between an
orchestrator whose turn is dead and one that knows to retry.

## 2. The mechanism

Three pieces, all in `crates/loomux-engine`.

**`TrackedMutex::lock_within(budget) -> Result<TrackedGuard, Busy>`**
(`lockwatch.rs`) is the explicit form: acquire, or give up after `budget` and say
who has it. The inner primitive is `parking_lot::Mutex`, chosen because std has
no timed acquire at all and `try_lock_for` *is* this operation; the manifest note
in `crates/loomux-engine/Cargo.toml` carries the reproduced dependency audit.

**`budget::read_budget(budget, f) -> Result<T, Busy>`** (`budget.rs`) is the
implicit form, and it is the lever the phase actually turns. It installs a
deadline on the *thread*; every `lock_safe()` underneath it — including calls
written next month, in code that has never heard of this module — becomes
`lock_within(remaining)`. The alternative the plan rejected was a `_within`
variant of ~30 registry read functions, which recreates the "did we remember to
bound this one" review dependency the epic's §2 is about.

`lock_safe()` returns a guard, not a `Result`, and that signature is what let
Phase 0 convert 448 call sites by changing a field's type. An infallible
signature has exactly two ways to report a failure: hang, or unwind. So a
timeout **unwinds**, with a typed payload caught at the `read_budget` frame that
owns the deadline, thrown by `std::panic::resume_unwind` — which does *not* run
the panic hook, so `obs` writes no crash log and the user is never told the app
died. It is a control-flow edge wearing an unwind's clothes.

**`budget::MutationScope`** is what makes that safe. At every mutating entry
point, a guard raises a thread-local depth; a timeout observed at depth > 0 does
not unwind at all — it breadcrumbs `lock-busy-in-mutation` and waits, unbounded,
exactly as it did before. A slow mutation is a stall, which Phase 0's watchdog
already reports with the holder's name attached. A mutation abandoned halfway
between two maps is corruption. The trade is deliberate and it only ever fails
toward the first.

**Nesting takes the tighter deadline**, and the frame id follows it: an inner
`read_budget(30 s)` inside an outer `read_budget(1 s)` does not extend anything —
the timeout carries the outer frame's id and the inner frame resumes the unwind
rather than catching it. Without that, a nested read could buy itself more time
than the poll tick that owes an answer.

### `Busy`, and the breadcrumb

```rust
pub struct Busy {
    pub lock: &'static str,          // the field name, as given to TrackedMutex::new
    pub waited: Duration,            // what THIS waiter actually paid, not the budget
    pub holder: Option<HolderInfo>,  // sampled without blocking; None if it moved mid-read
    pub waiters: usize,              // others still blocked, not counting this one
}
```

`Display` renders one line, and it is a public contract because it reaches an
agent's context and a human's screen:

```
`agents` held 42.1 s by src/orchestration/mod.rs:41942 (thread 7), 3 waiters; waited 5.0 s
```

The plan's sketch prefixed this with `registry busy: `. Dropped, because every
caller that renders it already says "busy" in its own first three words, and
`loomux busy: registry busy: \`agents\` …` is a sentence nobody would write on
purpose.

Each `Busy` breadcrumbs `lock-busy` **once per (lock, hold)** — edge-triggered on
the hold's own generation counter, like `queue_pressure`'s notices. A wedged
registry has every thread in the app queued behind one hold, so a breadcrumb per
waiter would turn the evidence trail into the noise it exists to cut through, and
would put a file write on each of their latency paths. A *second* hold of the same
lock that also goes busy is a new edge and does report — keying on the lock alone
would go silent forever after the first incident.

`Busy::retry_after_ms()` is a flat constant, not a prediction. Nothing here knows
when the holder will release, and the obvious derivation is worse than useless: a
number scaled down from how long the lock has already been held says "try again
sooner" exactly when the evidence says the opposite.

## 3. The budgets

Six, in one place (`budget.rs`), because a budget scattered across its call sites
is a policy nobody can review as a whole.

| constant | value | paid by | on expiry |
| --- | --- | --- | --- |
| `POLL_LOCK_BUDGET` | 1 s | each publisher section | keep the previous value, set `meta.partial` |
| `TICK_LOCK_BUDGET` | 5 s | a cadenced loop's entry acquisition | skip the tick, breadcrumb once |
| `MCP_AUTH_BUDGET` | 5 s | every MCP request, before dispatch | JSON-RPC `-32001`, retryable |
| `MCP_READ_BUDGET` | 15 s | a read-only MCP tool | `isError` result, nothing executed |
| `MCP_MUTATE_DEADLINE` | 30 s | the handler's WAIT for a mutating tool | "still executing", do not re-issue |
| `COMMAND_READ_BUDGET` | 10 s | a human's one-shot read command | the command's existing empty degrade |

The shape of the reasoning is the same in each case: the budget is set by **what
the caller does with the answer**, never by how long the work "should" take.

`POLL_LOCK_BUDGET` is 1 s because the publisher's own cadence is the recovery — a
section that misses this pass is retried next pass, and waiting longer buys a
fresher number at the cost of the tick it exists to serve. `TICK_LOCK_BUDGET` is
5 s because that is already the threshold past which a hold is *reportable*
(`DEFAULT_HOLD_WARN_MS`), so a tick skips only when something is independently
known to be wrong. `MCP_AUTH_BUDGET` is the tightest of the MCP three because it
is paid by every request including `ping` — it is the constant that answers
#1606's measured hole. `MCP_MUTATE_DEADLINE` is a deadline on the **wait**, never
on the work; see below.

### The two MCP shapes

> **Status:** the two shapes below are specified here and land with this PR's
> `mcp.rs` half, which waits on #1625 (Phase 1) merging — they are not in the
> engine-only half. This paragraph goes when they do.

Both are contracts an agent's model reads, so the wording is part of the design
rather than a message someone can reword later.

**Auth, and read tools that run out of budget.** Token resolution runs under
`read_budget(MCP_AUTH_BUDGET)`. `Busy` becomes a JSON-RPC error — protocol level,
because at that point the caller is not yet known:

```json
{"code": -32001, "message": "loomux busy: <Busy>; retry",
 "data": {"retryable": true, "retry_after_ms": 5000}}
```

A read tool that runs out of `MCP_READ_BUDGET` answers an `isError: true`
**result** rather than a protocol error, because MCP separates the two and a busy
read is an execution failure, not a malformed request — and the result shape is
what reaches the model's context as something it can act on:

```
loomux busy: <Busy>. Nothing was executed; retry in ~5 s.
```

"Nothing was executed" is load-bearing and it is true by construction: a read
tool that unwound took no lock it still holds and wrote nothing (§4).

**Mutating tools are deliberately NOT unwound.** A mutating tool that has taken
locks may already have mutated, so it runs to completion on a helper thread and
the handler waits `recv_timeout(MCP_MUTATE_DEADLINE)`. On timeout the caller gets:

```
<tool> is still executing after 30 s (waiting on `agents`, held 47 s by …).
It WILL complete; do NOT re-issue — verify with <read tool> first.
```

The late completion is audited with `late: true`. The rejected alternative was a
deadline around the body with the late result discarded, which produces **double
execution** when the agent retries a non-idempotent tool — the worst possible
outcome for `spawn_agent`. Exactly-once beats a tidy timeout.

## 4. Rider R1: why abandoning a read path is safe

The orchestrator made this blocking, and rightly: an unwind through arbitrary
read code is only safe if no read path is in the middle of writing something when
it fires. Two halves — the writes, and the guards.

### 4.1 The criterion, and why it is narrower than it first looks

An unwind fires **only** at a `lock_safe()` acquisition that runs out of budget,
at mutation depth 0. So a write `W` is torn only if, in the same dynamic frame,
there is a tracked-lock acquisition **between `W` and the point at which `W`'s
invariant is complete**.

A write with no acquisition after it is *not torn* — it is simply not reached,
which is the same outcome as any other early return and is what every one of
these functions already tolerates. That is what makes the audit finite: the
question is not "does this read path write?" but "does it write and then
acquire?".

The read-path set is closed and chosen here — publisher sections, MCP token
resolution, MCP `ToolKind::Read` arms, and the enumerated read commands — so the
audit is over their transitive writes.

**`resolve_token`** — the frame #1606's hole actually blames — is a pure read:
three acquisitions (`by_token`, `agents`, `groups`), zero writes. Nothing to
tear. This is why `MCP_AUTH_BUDGET` is the safest of the six as well as the most
valuable.

**`default_branch_within`** (`mod.rs`) reads the memo under one acquisition,
releases, spawns `git`, then re-acquires to insert. The insert is a single leaf
acquisition with nothing after it: an unwind there discards the subprocess result
and leaves the memo untouched, and the next caller re-runs the ladder. Cost: one
wasted `git`. The memo's invariant is `repo -> (stamp, answer)` and a missing
entry is a legal value of it — it is the initial one.

**`group_usage_memoed`** (`mod.rs`) is the one the rider names, and it has three
unwind points:

- at `cell.lock_safe()`, after `or_insert_with` has put an **empty** cell in the
  map. That is byte-identical to what the next caller would create; the map's
  invariant is "group -> its cell" and an empty cell satisfies it.
- inside `compute_group_usage`, with the cell guard live. `TrackedGuard::drop`
  runs as the unwind passes (§4.2), the cell keeps its **previous** value, and
  the memo is derived state: stale or absent costs a recomputation, never a wrong
  answer.
- at `*slot = Some(..)` itself — unreachable, because the tuple is fully built
  before the assignment and the assignment takes no lock.

**`merge_usage_snapshots`** (`mod.rs`) is the one real hazard, and it is *scoped*
rather than argued away. It is reached from `group_usage` — an MCP read tool and
a polled read — and it performs a durable whole-file write of `usage.json`. Today
nothing acquires after `atomic_write` on the success path, so it happens to be
untearable; that is a fact about this body, not a property of it, and the failure
path re-reads. It takes:

```rust
let _guard = self.usage_lock.lock_safe();   // still BOUNDED: nothing written yet,
                                            // so a wedged usage_lock still answers Busy
let _mutation = MutationScope::enter();     // from here on, no unwind
```

The order is the whole point. Entering the scope *before* the entry acquisition
would make a read path wait unbounded on `usage_lock` — reintroducing precisely
the bug this phase removes. The scope covers only the region in which a write is
in flight. `load_usage_snapshots`' corrupt-file branch (`fs::rename` followed by
an `audit()` that takes `AUDIT_LOCK`) is the one non-`atomic_write` write on a
read path, and it is inside this scope; it has no other caller.

### 4.2 `TrackedGuard::drop` during an unwind

The rider's second half. Rust drops live locals as an unwind passes through them,
so a guard held when some deeper acquisition times out gets its `Drop` body: the
generation counter goes odd -> even, the lock reads FREE to the watchdog, and the
inner guard's own drop releases the mutex.

The property that makes this true rather than hopeful is that **`Drop` cannot
panic**. Its body is one clock read, four relaxed loads, four relaxed stores, one
release store (`done_pending`) and one release read-modify-write (`generation`) —
no allocation, no indexing, and no arithmetic that can overflow
(`saturating_sub`). A panic in a drop during an unwind is an abort; this one has
nothing to panic with.

(The store and the read-modify-write are counted apart rather than lumped as "two
release read-modify-writes", which is what this paragraph said before rebasing
onto #1625 — #1608 corrected that figure on `TrackedGuard::drop` itself, and the
correction is load-bearing for the same reason it was there: this body runs with
the mutex still held, so what it costs is what every waiter behind it pays.)

Pinned, not asserted: `budget::tests::an_unwind_leaves_no_tracked_lock_held`
holds one lock, times out on a second, and after the `Err` checks both that the
first is re-acquirable **and** that `held_locks()` no longer names it — the second
being the watchdog-visible half, which is what would otherwise report a holder
that no longer exists.

### 4.3 The residual, and the detector that bounds it

§4.1 enumerates the writes on read paths **today**. What an enumeration can never
cover is the next edit: a write added to a function some read path calls, with
nothing to say so. That is the "did we remember" review dependency the epic's §2
is about, and leaving it as prose would be this document repeating the mistake it
describes.

Every durable orchestration state file is written through one door —
`fsatomic::atomic_write` — so that door notices:
`budget::note_durable_write` counts, and breadcrumbs `write-on-read-budget`, when
a durable write happens inside a `read_budget` frame and outside any
`MutationScope`. `budget::unscoped_durable_writes()` makes it assertable.

**It reports; it does not refuse.** A panic there would be a guard that blocks a
build, and the rule in `CLAUDE.md` is that a refusing guard ships only after
running clean over known-good subjects — which cannot be established for a write
population nobody has enumerated. A counter a test can assert on, plus a bounded
breadcrumb trail a field report carries, is what is available honestly.

Two things it does not see, stated here rather than left to be discovered:

- **Writes that are not `atomic_write`** — `fs::rename`, `fs::write`,
  `fs::create_dir_all`. The one such site on a read path today is named in §4.1
  and is inside a scope.
- **Non-durable writes** — in-memory registry state mutated on a read path. The
  memos in §4.1 are the known instances and are argued individually; a new one
  would be invisible here.

Giving `atomic_write` this call is `fsatomic`'s first outward edge, which
contradicted a claim its own header, the engine `lib.rs` account, and
`engine-extraction.md` all carried; all three are corrected in the same commit.
The boundary argument is untouched — still `std::fs` only, still nothing a
headless daemon cannot link.

## 5. What this does not do

- **It does not make the registry available.** A wedged lock is still wedged; the
  phase converts silence into a bounded, labelled, retryable answer. Phase 3 is
  what reduces the lock surface so the wedge becomes less likely.
- **It does not bound mutations.** By design (§2). A mutating path under
  contention still waits, and Phase 0's watchdog is what reports it.
- **It does not bound the per-pane delivery locks.** Waiting on a busy CLI is the
  feature (#1600 P6), and `mq_state_lock` keeps its existing `MQ_CMD_TIMEOUT`
  rather than gaining a second bound (X4).
- **It adds no source-scan guard.** The epic's §2.2 is that every guard added for
  the last four hangs described the previous failure exactly and none of them
  caught the next one. The enforcement here is a liveness test — `tests/liveness.rs`
  L2a-L2d, which ask whether the app still answers while something is stuck —
  plus the runtime detector in §4.3.
