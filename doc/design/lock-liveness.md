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

Phase 3a (#1610) adds the other half of the same object — whether the wait can
end at all, rather than how long it lasts — and lives in
`doc/design/lock-order.md`. The one thing it changes here is `Busy`, which gains
a `kind`: see §3.

---

## 1. What was unbounded, and what a bound buys

`lock_safe` is an infallible acquire: it returns a guard, so it has nowhere to
report a refusal and can only wait. Until #1609 there was no timed form of it
anywhere in the tree, so **a caller's cost was set by a lock's worst holder,
not by its own body.**
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
    pub kind: BusyKind,              // Timeout, or Reentrant (#1610)
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

### `BusyKind::Reentrant`, and why it is not `Busy::Reentrant`

Phase 3a (#1610, `doc/design/lock-order.md`) added a second reason an
acquisition can be refused: **this thread already holds the lock**. The plan's
sketch spells the answer `Busy::Reentrant`, which reads as an enum variant; it
is a `kind` on the struct instead.

The reason is that the two answers carry the same evidence. A re-entrant
refusal still names the lock, the site that holds it and how long that hold has
run — the "holder" is simply this thread's own earlier frame, which is precisely
the field a reader needs, and the one that turns "you deadlocked" into "you
deadlocked *there*". Splitting the struct in two would have duplicated all four
fields to distinguish them and rewritten every consumer — `mcp.rs`'s `rpc_busy`
and `busy_tool_text`, `views.rs`'s fallbacks, `budget.rs`'s unwind payload — to
match on a shape whose halves are identical.

Two consequences are worth stating rather than leaving to be found.

**`Display` branches.** The timeout wording ends `, 3 waiters; waited 5.0 s`,
which for a re-entrant refusal would be true and completely misleading — nothing
waited, and nothing is going to free up, because the caller is standing on the
lock itself. So a re-entrant `Busy` renders its own sentence:

```
`tasks_lock` is already held by this same thread, taken 0.0 s ago at src/orchestration/mod.rs:27011 — a re-entrant acquisition would deadlock, so it was refused
```

**`retry_after_ms` is the same constant for both.** A re-entrant refusal is a
defect that retrying the *same call* cannot clear — but the caller that receives
one is an MCP request or a cadenced tick, and its retry is a fresh request on a
fresh thread with an empty held-lock stack, which is the only sense in which any
`Busy` was ever "retryable". Nothing here knows enough to say otherwise, and a
`retry_after_ms` of 0 would say "hammer it".

`docs/orchestration.md`'s "when an agent reports `loomux busy`" section
describes the answer a *user* can see, and its core claim — "nothing was
executed, and you can try again" — is still true of every `busy` answer,
re-entrant refusals included: the refusal happens *instead of* the acquisition,
so nothing downstream of it ran.

What changed under it is which paths can produce one. Until #1702 a re-entrant
refusal came only from `lock_within`, and `lock_safe` hung the app instead, so
no shipped read path could return one. Now `lock_safe` refuses too, so a
re-entrant defect on a read path reaches a user as a `loomux busy:` line naming
both sites rather than as a freeze — and one on a path with no budget frame
reaches them as a crash log plus a `tick-panicked` breadcrumb. That page gained
a paragraph for the second shape, because it is a *new* message a user can see;
the `busy` text itself did not change.

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
| `TICK_LOCK_BUDGET` | 5 s | a cadenced loop's gate probe | skip the tick, breadcrumb once |
| `MCP_AUTH_BUDGET` | 5 s | every MCP request, before dispatch | JSON-RPC `-32001`, retryable |
| `MCP_READ_BUDGET` | 15 s | a read-only MCP tool | `isError` result, nothing executed |
| `MCP_MUTATE_DEADLINE` | 30 s | the handler's WAIT for a mutating tool | "still executing", do not re-issue |
| `COMMAND_READ_BUDGET` | 10 s | a human's one-shot read command | the command's existing empty degrade — **undisclosed**, see below |

The shape of the reasoning is the same in each case: the budget is set by **what
the caller does with the answer**, never by how long the work "should" take.

### The tick gate probes, rather than bounding "the entry acquisition"

The plan says to bound each cadenced tick's own entry acquisition. That is not
a usable instruction here, and `OrchRegistry::tick_gate` deviates from it
deliberately: three of these ticks enter through
`agent_output_totals`/`attention_inputs`, whose first acquisition is `app` — a
trivial cell that says nothing about whether the registry is wedged — and then
park on `agents` two frames down. Bounding the lexically-first lock would give
a gate that passes and a tick that parks anyway.

So the gate probes `agents` and `groups`, the registry's two core maps, and
releases them at once. It is not mutual exclusion: the question is "can the
registry serve anyone right now", not "may I have this lock".

What it buys is that a cadenced loop does not ADD a parked thread to an
already-wedged registry — the accumulation half of §1.2. What it does not buy
is a bound on a tick that wedges midway: that tick waits, because its body
mutates, and Phase 0's watchdog is what reports it.

`POLL_LOCK_BUDGET` is 1 s because the publisher's own cadence is the recovery — a
section that misses this pass is retried next pass, and waiting longer buys a
fresher number at the cost of the tick it exists to serve. A busy section keeps
its previously published value AND that value's age: `age_ms` becomes the age of
the group's stalest part, so `viewstale.ts`'s existing "partly stale" label is
correct with no frontend change, and a panel can never read current while
showing a frozen number. `TICK_LOCK_BUDGET` is
5 s because that is already the threshold past which a hold is *reportable*
(`DEFAULT_HOLD_WARN_MS`), so a tick skips only when something is independently
known to be wrong. `MCP_AUTH_BUDGET` is the tightest of the MCP three because it
is paid by every request including `ping` — it is the constant that answers
#1606's measured hole. `MCP_MUTATE_DEADLINE` is a deadline on the **wait**, never
on the work; see below.

### The one degrade that does not disclose itself

Every other row above tells the reader something happened: the publisher sets
`meta.partial`, the MCP answers a busy error or an `isError` result, a skipped
tick breadcrumbs. The human one-shot read commands do not. A `Busy` there
returns the same empty value the command already returns for an unvalidated
group id — an empty board, a zero unread count — and the only trace is a
breadcrumb the human never sees.

That is a real gap rather than a considered degrade, and it is left open
deliberately: `command_group` gives these commands no error channel, so
disclosing it means adding a meta channel to each of the six, which is a wire
change larger than this slice should make on its own initiative. It is the
same class the publisher path closes with `partial`, one layer over.

### The two MCP shapes

Both are contracts an agent's model reads, so the wording is part of the design
rather than a message someone can reword later.

**Auth, and read tools that run out of budget.** Token resolution runs under
`read_budget(MCP_AUTH_BUDGET)`. `Busy` becomes a JSON-RPC error — protocol level,
because at that point the caller is not yet known.

`tools/list` is the **second** protocol-level producer of this code: it runs under
`read_budget(MCP_READ_BUDGET)` (its `lock_menu`/`manager_block` both reach
`groups`) and has no tool result to attach a busy to either. Both render through
`rpc_error`, which attaches the `data` block for this code ITSELF — so there is
one envelope shape per code by construction rather than by each producer
remembering (#1609 review round 2, B2):

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

**Mutating tools are deliberately NOT unwound by a budget.** A mutating tool
that has taken locks may already have mutated, so it is left running on a helper
thread and the handler waits `recv_timeout(MCP_MUTATE_DEADLINE)`. On timeout the
caller gets:

```
<tool> is still executing after 30 s (waiting on `agents`, held 47 s by …).
Do NOT re-issue it: it is still running, and a second call would run it twice
— verify with <read tool> first.
```

That message used to end *"It WILL complete; do NOT re-issue"*. The completion
half is gone (#1702): a re-entrant `lock_safe` panics the helper thread rather
than parking it, so a mutating tool can end without a result, and the sentence
an agent actually reads was the last surviving instance of the claim the
at-most-once correction retracted everywhere else. What it kept is the half that
is still true and still load-bearing — do not re-issue, because a second call
runs a non-idempotent tool twice.

The late completion is audited with `late: true`. The rejected alternative was a
deadline around the body with the late result discarded, which produces **double
execution** when the agent retries a non-idempotent tool — the worst possible
outcome for `spawn_agent`. At-most-once beats a tidy timeout.

**At most once, not exactly once** (#1702). This section used to say
"exactly-once", and the two halves of that have different strengths. Nothing can
make a mutating tool run TWICE — that is what the deadline-on-the-wait buys, and
nothing since has touched it. Completion is the half that is conditional: a
re-entrant `lock_safe` panics the helper thread instead of parking it (§4.1's
narrowing, `lock-order.md` §2.1), so the tool can end without a result. That is
not a hole in the design, it is the design working — a thread that would
otherwise have wedged the registry forever is released — but it means the
handler owes the caller a third answer, and `worker_died_text` is it: an
`isError` naming the crash log and the read tool to verify with, saying the tool
**may have** partially executed. `docs/orchestration.md` carries the
user-facing half of the same correction.

### What the busy fallback inherits, and the one case it cannot

CLAUDE.md: *a cache or snapshot placed in front of per-item reads inherits
everything those reads answered* — enumerate what the replaced path could do
that the new one cannot, because the miss is SILENT.

The replaced path here is not a per-item command; it is the SAME section read,
run unbounded. So the enumeration is short, and it has **two** gaps — one per
granularity, and the second arrived with the view-tier fix in review round 1
(N1):

| the section read could... | the busy fallback... |
| --- | --- |
| answer for a live group | inherits that group's previous value |
| answer for a restored, strip-leased group | inherits its previous value |
| answer on a group's FIRST pass | **has nothing to inherit** — group withheld |
| answer a VIEW-TIER section for a group whose previous entry was strip-only | **has no previous tier to inherit** — tier withheld (`view: None`) |

The third row is the sharper of the two, and the class it bites is the one
#1625 round 2 already found: a restored-but-not-resumed group arrives through a
strip lease and has no prior entry by construction, so its first published pass
is exactly the pass that can hit a busy section.

The fourth is the same shape one granularity down: a group bound to a tab but
never view-leased has a previous entry with `view: None`, so a busy tier
section has nothing to fall back to. Its three booleans would otherwise
fabricate `false` — "this group is not paused" — which is an assertion a human
acts on. Both are answered the same way, and it is the same answer the first
row already relies on: absence is a state both payload builders render, and an
entry full of defaults is strictly worse because it asserts something.

Publishing it anyway would put an entry in the snapshot whose every busy section
is `Null` and whose `computed_at` is this pass — fresh, so no stale badge — which
renders as *this group has nothing*. That is an assertion, and a false one.

So a group that goes partial with nothing to inherit is **withheld**: not
published at all, and retried next pass. Absence is not a new degrade — it
already means "a group created since the last pass; keep your previous render and
ask again shortly" to both payload builders. `publish_pass_at` carries a withheld
id forward from the swap-time snapshot if a nudge published one meanwhile, so
withholding can never itself lose a group.

Pinned by `a_first_pass_that_goes_partial_publishes_nothing_rather_than_nulls`
(`tests/liveness.rs`), whose discriminating half is a second registry proving an
unheld first pass DOES publish.

## 4. Rider R1: why abandoning a read path is safe

The orchestrator made this blocking, and rightly: an unwind through arbitrary
read code is only safe if no read path is in the middle of writing something
when it fires.

**Round 1 of review found the first answer to this insufficient, and the way it
was insufficient is the reason §4.1 is now a mechanism rather than a list.** The
original answer enumerated the writes on the read paths and argued each one
safe. The enumeration declared its own scope as including "MCP `ToolKind::Read`
arms" and then never looked at them: four of the seventeen wrote durably, one
(`check_mail`) *consumingly*, and the `MutationScope` the note quoted for the
one hazard it did enumerate was not in the tree at all. An enumeration is a
claim about today's call graph, re-verified by nobody, and it failed exactly
where a claim like that fails — in the part somebody assumed rather than read.

### 4.1 The rule: a durable write seals its frame

An unwind can fire **only** at a lock acquisition that runs out of budget. So a
write is never torn by itself — it is torn by an acquisition that comes *after*
it, unwinding past the code that would have completed its invariant.

That gives the rule, and it is the whole of the safety argument:

> **The first durable write inside a `read_budget` frame SEALS that frame.**
> From there to the end of it, a timeout waits instead of unwinding — exactly as
> inside a [`MutationScope`], and for the same reason.

`budget::note_durable_write` is where it happens; `budget::unwind_forbidden` is
what the acquisition path asks. Everything *before* the first durable write
keeps its bound, so the common case — a read that reads — is unaffected, and a
read path that writes gives up its bound only for the region that needs it. It
fails toward a stall, which Phase 0's watchdog reports with the holder named,
never toward a torn write.

**The one narrowing: a re-entrant acquisition unwinds a sealed frame anyway**
(#1702). The seal's whole argument is the sentence above it — *"a mutation that
is slow is a stall, a mutation abandoned halfway between two maps is
corruption"* — and it is a good trade because a stall **ends**. Every other
thing the seal makes a frame wait for ends: a slow holder finishes, a timed
acquire eventually gets its lock, and the watchdog names whoever is taking so
long. A re-entrant acquisition is the one case where waiting does not end at
all: `parking_lot::Mutex` is not re-entrant, so the thread that reaches
`inner.lock()` on a lock it already holds parks permanently, and nothing that
runs later can release it — the frame is the holder. So the choice is not "tear
now or complete later", it is "tear now or never complete", and the seal yields:
`lock_safe` unwinds through it with a `BusyKind::Reentrant` `Busy`.

That is not a hole in R1, it is R1's own accounting used honestly. `WROTE` is
still set, so the unwind is **counted as a tear** (`budget::torn_writes`) and
breadcrumbed `budget-torn-write` — which is precisely why that counter is
measured off `WROTE` and not off `SEALED` (§4.1's own note): a tear through the
seal has to be countable, or the narrowing would be invisible in exactly the
tree where it fired. A wedge is counted as nothing at all, which is how #1702
ran for four betas. A named, counted tear on a path that is already a certain
deadlock beats an unnamed wedge, and `a_reentrant_acquire_unwinds_a_sealed_frame_and_counts_the_tear`
(`lockwatch.rs`) is what pins both halves: the unwind, and the count.

The narrowing is exactly this wide. A budget TIMEOUT inside a sealed frame still
waits — `a_durable_write_seals_its_budget_frame_so_a_later_timeout_waits`
(`budget.rs`) still holds for every case the seal was written for, because a
timeout is a stall and this is not.

**What the narrowing did change about that test is its INSTRUMENT, and the
reason is worth keeping.** Its "this frame did not tear" assertion was an
equality on the process-global `torn_writes()`, which is sound only while
*nothing in the whole binary ever tears*. That was true until this narrowing
existed, and the moment a sibling test produced a legitimate tear the assertion
started failing on whichever platform happened to schedule the two in the wrong
order — measured: `left 1, right 0` on ubuntu and windows, green on macos. An
absence stated on a shared counter is a claim about the whole process, not about
your frame. It is now read from `thread_seal_counts()`, the per-thread pair this
module built for exactly that, which makes the pin stronger rather than looser:
an exact equality that cannot race, in place of one that could only ever have
held by luck. The global keeps its own witness — a monotonic floor in
`a_reentrant_acquire_unwinds_a_sealed_frame_and_counts_the_tear` — because a
field-report counter nothing asserts on is a counter that can quietly stop
counting.

**Why not the other candidate rule.** The alternative was to make every tool arm
that writes into a `ToolKind::Mutate`. That is right where a tool genuinely
mutates and is done below — but it cannot be the whole answer, because it only
reaches writes that are behind a *tool*. The snapshot publisher's `usage.json`
merge is on a read path with no tool anywhere near it. Nor could a hand-placed
`MutationScope` per site be: that is the "did we remember" review dependency the
epic's §2 is about, over seventeen arms plus whatever is added next month — and
the first version of this note is the evidence, having missed four of them.

**Why seal at the write and not at the function's entry.** Sealing an entry
point gives up the bound on every path through it, including the ones that never
write. Sealing at the write is the minimum window that closes the hazard.

### 4.2 The classification, re-derived from what the arms do

The `ToolKind` table was originally written from what tool NAMES sound like,
which is the same defect this repo has a rule against for source-scanning
guards. Re-derived from the arms:

| tool | what it does | kind |
| --- | --- | --- |
| `check_mail` | marks every message read, prunes, replaces `mailbox.json`, then takes `app` and `AUDIT_LOCK` | **Mutate** |
| `queue_orphans` | publishes a recovery latch, then a two-phase persist/deliver cascade | **Mutate** |
| `list_locks` | `with_locks` → `table.sync(declared)`, which drops undeclared resources including live holders, then audits | **Mutate** |
| `group_usage` | merges and replaces `usage.json` as a cache refresh | Read, sealed |
| the rest | read registry or on-disk state | Read |

`check_mail` is the one worth dwelling on: its own doc calls it *"the manager's
consuming read"*, and the registry it belongs to exists to make *"a message
silently consumed by nobody"* impossible. Classified as a Read and unwound after
its write, it produced precisely that — mail marked read and pruned on disk,
with the caller told nothing had executed.

`group_usage` stays a Read deliberately. Its write is a durable *cache refresh*
rather than the point of the call, and the seal is what makes it safe; putting
every usage read on the 30 s mutate deadline would be a heavy answer to a hazard
the floor already closes.

`pr_checks` was in the Read set and is not a tool at all — `tool_defs` registers
no such name. A dead row classifies nothing, and a typo'd entry degrades to
`Mutate` silently, so the classification test now takes its population from what
`tools/list` actually returns.

### 4.3 Which writes seal: replace, not append

The claim this note used to make — *"every durable orchestration state file goes
through one door"* — was false, and this PR's own L2a fix made it more so by
moving the `tool-call` audit line inside the read budget. The measured set of
durable-write primitives in the orchestration tree is `fsatomic::atomic_write`,
`append_audit` / `append_ledger_line` (append-only, via `OpenOptions`), and a
small number of direct `fs::write` / `fs::rename` calls.

They do not all need the same treatment, and the line between them is what
*tearing* means:

- **A REPLACE destroys the prior value.** If the follow-up work is abandoned,
  what is left on disk is a world nothing else agrees with. These seal:
  `atomic_write` (every state file), and `load_usage_snapshots`' corrupt-file
  `fs::rename`, which moves the live file aside and is followed by the `audit`
  acquisition that records why.
- **An APPEND leaves a complete record with nothing depending on it.** These do
  not seal, and that is deliberate rather than an omission: `append_audit` runs
  for *every* tool call including every Read, inside the budget, so sealing it
  would disarm the read budget for the entire MCP surface — the bound would be
  live only until the first audit line, which is to say never.

  **`append_audit` is not purely an append, and classifying it by its name
  would be the defect §4.2 is about** (#1609 review round 2, N-B). It reaches
  `rotate_audit_locked`, which does `fs::rename(audit.jsonl -> audit.1.jsonl)`
  — destroying the previous generation, which is a REPLACE by the definition
  above. It is still right not to seal it, but for a reason that has to be
  stated rather than assumed: **no tracked acquisition sits between that rename
  and the append that follows it**, so there is no point at which a timeout
  could fire and leave the rotation half-applied. The exemption is about the
  shape of the code, not the shape of the name.

`note_agent_ack`, which rider R1 named alongside `audit`: one acquisition, an
in-memory `max`, nothing after it. Nothing to tear.

### 4.4 `TrackedGuard::drop` during an unwind

Rider R1's second half. Rust drops live locals as an unwind passes through them,
so a guard held when some deeper acquisition times out gets its `Drop` body: the
generation counter goes odd → even, the lock reads FREE to the watchdog, and the
inner guard's own drop releases the mutex.

The property that makes this true rather than hopeful is that **`Drop` cannot
panic**. Its body is one clock read, four relaxed loads, four relaxed stores, one
release store (`done_pending`) and one release read-modify-write (`generation`) —
no allocation, no indexing, and no arithmetic that can overflow
(`saturating_sub`). A panic in a drop during an unwind is an abort; this one has
nothing to panic with.

(The store and the read-modify-write are counted apart rather than lumped as
"two release read-modify-writes", which is what this paragraph said before
rebasing onto #1625 — #1608 corrected that figure on `TrackedGuard::drop`
itself, and the correction is load-bearing for the same reason it was there:
this body runs with the mutex still held, so what it costs is what every waiter
behind it pays.)

Pinned, not asserted: `budget::tests::an_unwind_leaves_no_tracked_lock_held`
holds one lock, times out on a second, and after the `Err` checks both that the
first is re-acquirable **and** that `held_locks()` no longer names it — the
second being the watchdog-visible half, which is what would otherwise report a
holder that no longer exists.

### 4.5 R1 is a test, not an argument

`budget::torn_writes()` counts frames that unwound after a durable write — the
tear itself. The seal makes it structurally zero, and
`no_read_tool_can_unwind_after_a_durable_write` (`tests/liveness.rs`) is what
asserts it: every tool the surface classifies as a Read, driven under a budget
with `app` held — the lock `write_mailbox` takes *after* its durable replace,
which is the shape the tear needs.

Three claims, kept apart because they are easy to conflate:

- the **sweep** drives every Read tool with a caller that can run it and asserts
  none tore. It is a REGRESSION GUARD and nothing more: no run has shown it
  failing, including the one cut to make it fail — see below;
- the **probe** beside it — a frame that deliberately writes and then hits a held
  lock — proves the instrument that would catch one is running, and it is the
  assertion in this test with a counterfactual. It replaced a
  population control that asserted "some tool wrote during the sweep", which
  failed on CI and was right to: no Read tool did, so the sweep's `torn == 0` was
  vacuous, and even had one written it would have been a fact about the fixture
  rather than a property;
- the **seal's own necessity** is carried by neither of those, but by
  `budget.rs`'s two seal tests, which redden when it is disarmed.

### What has actually been observed

This section exists because the list above once claimed a counterfactual that
had never run (#1609 review round 2, B1) — a claim now corrected in the bullets
themselves rather than only refuted here, which is what round 3 found still
outstanding. The scratch round in question
mutated the classification AND the seal, so the engine binary reddened first
and `cargo` never reached `tests/liveness.rs` — the sweep's behaviour on that
tree was simply unknown, while the note said it had been shown.

The stageable form is the same mutation plus `#[ignore]` on the two `budget.rs`
seal tests, so the engine binary passes and the integration binaries run.
That round was cut (`scratch/1609-r3-red`, run **33257747970**) and it settles
the question in a way worth stating exactly, because it is not the answer the
retracted sentence claimed.

**`tests/liveness.rs` was reached** — the engine binary reports
`425 passed; 0 failed; 2 ignored`, so the `#[ignore]`s did their job — and L2g
**failed**:

```
test no_read_tool_can_unwind_after_a_durable_write ... FAILED
panicked at src-tauri/tests/liveness.rs:1465:
a frame that wrote durably and then hit a held lock UNWOUND. The seal is not
engaging, so `torn == 0` above measures a mechanism that is not running
test result: FAILED. 14 passed; 1 failed
```

**The assertion that fired is the PROBE, at line 1465 — not the sweep.** The
sweep's own `assert_eq!(torn, 0, ..)` runs first, at line 1438, and it PASSED on
the defective tree: `check_mail` was back in the Read set, unsealed, and driven
by a caller that lists it, and the sweep still saw no tear.

So the honest division is the one §4.5 already draws, now measured rather than
reasoned:

- the **probe** is the assertion with a counterfactual. It is what reddens when
  the seal stops engaging, and it is why L2g is a test rather than a formality;
- the **sweep** does NOT catch a misclassified writer, on the evidence. It
  remains a regression guard — it drives every Read tool with a caller that can
  run it and asserts none tore — but no run has shown it failing, and this round
  is the one that would have.

Why the sweep stayed green is not established here and is deliberately not
guessed at; what is established is that it did. The claim this section replaced
said the sweep catches such a writer, and the round cut to prove it proved the
opposite.

**What is still not covered**, stated rather than left to be found:

- `note_durable_write` is called from the replace-shaped writers named in §4.3.
  A new durable write that reaches neither of them is invisible to the seal, and
  the sweep would only catch it if it were on a Read arm and tore during that
  test.
- The seal is per frame and per thread. Work handed to another thread inside a
  read frame does not inherit it; nothing on these paths does that today.
- **The converse of per-frame scoping**, which the line above reads as a pure
  virtue (#1609 review round 2, N-C): `read_budget` restores both `SEALED` and
  `WROTE` on exit, so an INNER frame's durable write leaves the OUTER frame
  neither sealed nor counted for its remaining acquisitions. A nested read
  frame whose inner half writes is therefore both tearable and invisible to
  `torn_writes()`. Vacuous today — the five production `read_budget` call sites
  (auth, `tools/list`, the read-tool arm, `read_command`, the publisher's
  `section`) are each outermost and none nests — but it is a property of the
  mechanism rather than of today's call sites, so it belongs in this list
  rather than in whoever first nests one.

## 5. What this does not do

- **It does not make the registry available.** A wedged lock is still wedged; the
  phase converts silence into a bounded, labelled, retryable answer. Phase 3 is
  what reduces the lock surface so the wedge becomes less likely.
- **It does not bound mutations.** By design (§2). A mutating path under
  contention still waits, and Phase 0's watchdog is what reports it. The one
  case a mutating or sealed frame does NOT wait for is a re-entrant
  acquisition, which is not contention at all — §4.1's narrowing (#1702).
- **It does not bound the per-pane delivery locks.** Waiting on a busy CLI is the
  feature (#1600 P6), and `mq_state_lock` keeps its existing `MQ_CMD_TIMEOUT`
  rather than gaining a second bound (X4).
- **It adds no source-scan guard.** The epic's §2.2 is that every guard added for
  the last four hangs described the previous failure exactly and none of them
  caught the next one. The enforcement here is a liveness test — `tests/liveness.rs`
  L2a-L2d, which ask whether the app still answers while something is stuck —
  plus the runtime detector in §4.3.

## 6. The hold side: re-entrancy, and why a budget cannot see it

Everything above bounds what a **waiter** pays. #1702 is the first field
incident produced by the other side of the same lock — an unbounded **hold** —
and it is worth its own section because the mechanism this phase shipped does
not merely miss that case, it is capable of describing it wrongly.

**What happened.** `OrchRegistry::attention_tick` took `agents` and then, inside
the loop over that guard, called `delivered_mask_lines`. That reaches
`delivered_prompt_record`, then `session_for_pty`, whose second line is
`self.agents.lock_safe()`. `TrackedMutex`'s inner primitive is
`parking_lot::Mutex`, which is not re-entrant, so the tick blocked on a lock it
was itself holding. Both halves of that nesting entered in the same commit
(#903), which is why the trace #1702 carries is a *single* hold observed at
38.8 s, 64.6 s, 99.0 s, 141.5 s, 163.0 s, 245 s and 336 s across separate
retries — never released, and re-forming within seconds of a restart, because
the trigger is ordinary state rather than a rare input: any agent that is
running, pty-bound, present in `by_pty`, and quiet past `ATTENTION_QUIET_MS`.

**Why §2's machinery could not save it, in either direction.** The tick runs
inside `tick_gate`'s `MutationScope`, so an expired budget takes §2's
"breadcrumb and wait, unbounded" arm — correctly, by that section's own trade,
since abandoning a mutation half-done is the worse failure. The hold is
therefore permanent by design once it forms. And the *other* arm is not a fix
either: at mutation depth 0 the same acquisition would unwind with a `Busy`
naming this thread's own hold, which is a false statement about a contended
registry rather than a report of a deadlock. **A budget converts a
self-deadlock into a lie, never into a rescue.** That is the argument for
treating re-entrancy as a shape to remove rather than a failure to bound.

**The rule, which is what generalises.** A registry lock may not be held across
a call that can take a registry lock. Not "across a slow call" — the cost is
irrelevant to this failure, and the site that produced #1702's minutes-long
holds does string work measured in microseconds. `session_for_pty`'s own doc
USED TO promise the narrower half of this — "`by_pty` is taken and RELEASED
before `agents`", wording this change removed — and that promise was worth
nothing to a caller that is holding `agents` before it arrives — a lock-order note describes what one
function does, where what a reader needs is what is true at the call site.

**The shape `attention_tick` now has**, and the shape any tick doing per-item
work should:

| phase | holds | may do |
| --- | --- | --- |
| 1. snapshot | one short registry hold | clone the fields the pass needs, nothing else |
| 2. compute | **nothing** | per-item work, file reads, anything that re-locks |
| 3. apply | one short hold of the maps it owns | in-memory decisions over phase 2's results |

`plain_pane_attention` was hoisted the same way in the same change. It was not
deadlocking — it holds neither `by_pty` nor `agents` — but it held two
attention maps across the same four-lock call, which is the identical shape one
edit away from being the identical defect.

**One signature carries the rule, so a later caller cannot re-create the
nesting by accident.** `delivered_mask_lines(pty, session)` takes the session
from its CALLER; it used to take a pty alone and resolve it internally through
`session_for_pty`, which meant every caller silently paid `by_pty` + `agents`
inside a function whose name promises a record read. That hidden acquisition is
the whole defect: `attention_tick` already HAD the session — it was holding the
map the resolution reads — and asked for it anyway. Now the tick passes
`AgentEntry::session_id` straight off its phase-1 snapshot and resolves nothing,
and a caller that has only a pty writes `session_for_pty` at its own call site,
where whether a lock is held is something a reader can see rather than infer.
`session_for_pty`'s doc says what it ACQUIRES for the same reason: its previous
"`by_pty` is taken and RELEASED before `agents`" was true of its body, useless
to its caller, and is the shape of note that let this ship.

**The bound, which is stated rather than added.** #1702's reported diagnosis was
that the mask reconciles a record proportional to a session's age, and that is
false; recording it here is the point of this paragraph, because the next reader
of the issue will otherwise add a cap that is already there.
`delivered_mask_lines` unions two drop-oldest records, each capped where it is
written — `DELIVERED_NOTICES_PER_PANE` (24) lines of the pane's notice record
and `DELIVERED_PROMPT_LINES_PER_SESSION` (16) of the session's prompt record —
so it returns at most 40 lines however many thousands of deliveries a session
has taken, and the maps holding them are capped by pane and by session as well
(`DELIVERED_NOTICE_PANES`, `DELIVERED_PROMPT_SESSIONS`), which is what bounds
the memory. The other operand is capped too: `attention_tail` reads only the
trailing `ATTENTION_SCAN_BYTES` (4096) of the ring. One agent's mask is
therefore about 100 rows against 40 records. **No second cap was added**, and
that is a decision rather than an omission: a cap over an already-capped record
could only narrow what the mask may claim, which is a change to #903 B2's
guarantees, and it would buy no bound that is not already held.

**How it is guarded, and what that guard does NOT cover.** The regression guard
is two rows in `tests/liveness.rs`:
`l6a_the_attention_tick_returns_on_the_state_that_deadlocked_it` and
`l6b_the_masks_the_tick_applies_equal_the_unbounded_computation`. They are
split because they redden for different reasons and a red evidences only the
assertion it reached: on the pre-fix tick L6a never gets past "did the call
return", so an equality assertion sharing its body would bank a coverage claim
no run ever produced.

Their fixture is the finding as much as the assertions are: the defect survived
four betas because *no test in this repo could build the shape*. `attention_setup`, the helper every attention test in
`orchestration.rs` uses, spawns agents with no pty, so `pty_id` is `None` and
the mask is never reached at all; the soak lane wedges a lock deliberately and
probes, which measures victims of a hold and never constructs a holder out of
ordinary state. `tests/common/mod.rs` builds the missing subject — pty-bound,
`by_pty`-mapped, session-bound, session-sized — as a generator, because the
siblings of this defect want the same subject.

**And there is now a class guard, which changes what this defect looks like.**
Phase 3a (#1698) merged while this slice was in flight, so `agents` carries
`lockorder::AGENTS` (510) and `by_pty` carries `BY_PTY` (500). The pre-fix tick
therefore fails one line EARLIER than the re-entrant acquire and for a
different stated reason: holding 510 and reaching for 500 is a descending pair,
so the checker panics naming both locks before the deadlock can form. The
timeout and the panic are the same defect through two instruments, which is why
L6a distinguishes them in its failure message rather than reporting one as the
other.

L6a refuses an observed `agents` hold of 50 ms or more, which is the figure a
deliberate 250 ms hold on the measured thread proves the sampler reports — the
assertion and the instrument's demonstrated sensitivity are deliberately the
same number, so the row cannot refuse something it has not been shown to see.
Between 50 ms and 250 ms the sensitivity is argued from the 500 us sampling
cadence rather than measured, and phase 1's true hold (microseconds) is below
the sampler's floor entirely: this is a CEILING, not a measurement of the hold.

That is a better failure than a hang, and it is not a substitute for this
section. The checker sees a pair of RANKED locks held at once; it does not see
a long hold, and it does not see the unranked ones — `app` and every `attn_*`
map are `TrackedMutex::new`, so the `if let` guard `run_attention` was holding
across an IPC emit and three further locks is invisible to it. Ranks bound the
ORDER of a nesting; §6's rule is that a registry lock should not span such a
call at all. Review is still the enforcement for that, as §5's last bullet says
of source scans, for the same reason.
