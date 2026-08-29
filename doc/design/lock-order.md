# The registry's lock order, as a runtime-checked rank

#1610, Phase 3a of #1600. The companion to `doc/design/lock-liveness.md`: that
note is about how long a waiter pays; this one is about whether the wait can end
at all.

## 1. What this closes

#1600 §3 opens on it, and `resolve_token`'s own comment says it out loud:

> locking them together would pin a lock order no other call site promises to
> respect

Seventeen mutexes on one struct, no declared order, and 448 acquisition sites.
Thirteen doc comments in `orchestration/mod.rs` *do* state an order, and every
one of them is true. None of them can fail a build.

That is §2.2's finding about this repo's guard culture applied to the one
property a source scan cannot see: an order is a fact about the sequence a
thread takes locks in at run time, and no amount of reading tells you what the
call chain three frames up was already holding. #1595's audit searched for a
two-lock inversion between `agents` and `groups` and concluded that nobody held
a lock pathologically long. beta6 is the counterexample.

Two failure shapes are in scope, and the second is the one the epic singles out
as invisible:

- **An inversion.** Thread A takes X then Y; thread B takes Y then X. A cycle,
  and it hangs only when the two race.
- **A re-entrant self-acquisition.** One thread takes X and, some frames down,
  takes X again. `parking_lot::Mutex` is not re-entrant, so this parks
  *permanently* — and it is one lock, so there is no cycle for an inversion
  search to find. #1600 §1.2 names it; no guard in the tree could see it.

## 2. The mechanism

`LockRank` is a `u32` where **smaller is outer**. `TrackedMutex::new_ranked`
gives a lock its rank; plain `new` leaves it unranked. Every acquisition path
consults a **per-thread stack of held locks** before it can block:

| what the checker sees | kind | in an ARMED build (debug, `cargo test`, the E2E lane) | in a SHIPPED build (release) |
| --- | --- | --- | --- |
| this exact lock is already on this thread's stack | **re-entrant** | panic naming both locks and both sites | **refused, never taken.** `Err(Busy)` with `BusyKind::Reentrant` from `lock_within`; from `lock_safe`, an unwind — to the `read_budget` frame if there is one, else a panic (§2.1) |
| the rank being taken is *strictly below* the innermost rank held | **inversion** | panic naming both locks and both sites | a `lock-order-violation` finding, then **acquire anyway** (§2.1) |
| equal ranks | not a violation | allowed — see §3 | allowed |
| the lock is unranked and something is held | a missing table row | a `lock-rank-unranked` finding, once per lock; not an error | same |
| anything else | — | nothing | nothing |

**The two kinds get different release answers, and that is the point of the
column rather than an inconsistency.** The shipped behaviour is decided per
detected KIND, not by one switch read at one place: `LOCK_ORDER_PANICS` chooses
the *shape* of the report, and what a violation does about the acquisition in
front of it is chosen by which violation it is. §2.1 is the argument; §6 is why
the earlier single-switch version of it was right for one kind and wrong for the
other.

The check is per-thread and **needs no second thread to fire**. That is the
whole reason it finds what a soak test cannot: it does not need the race to
happen, only the order that permits it. One test process taking `groups` then
`agents` once is enough to say the ordering fact exists.

**Both ends, in both surfaces.** A report naming only the lock being taken
sends the next reader back into a 54,000-line module to guess what was already
held, which is the state #1600 §2.3 describes. So both surfaces carry the
acquiring lock's name, rank and `#[track_caller]` site, and the rank and site of
the hold it collided with.

The one asymmetry, stated because a reader will notice it: the panic also names
the outer lock by NAME and the breadcrumb does not. The panic composes on the
spot, with the held-lock stack entry in hand; the breadcrumb composes a second
later on the watchdog thread, from a slot that keeps a `&'static Location` (valid
for the whole program) rather than a borrowed name from a `LockState` that may
have been dropped. The site is the more useful half anyway — it is the line to
go and read.

### 2.1 An inversion fails open. A re-entrant acquisition is refused.

The two verdicts got the same answer in 3a and it was wrong for one of them
(#1702). They are different facts:

- an **inversion** is a *possible* deadlock. It needs a second thread to take
  the same two locks the other way round, at the same moment. Nothing hangs
  until that happens, and it may never happen on the path in front of you;
- a **re-entrant acquire** on a non-reentrant mutex is a *certain* deadlock. It
  needs nothing and nobody: the thread that reaches it parks permanently, every
  time.

So an inversion still fails open, and it is not timidity. A shipped build
records the finding and then does exactly what it would have done anyway;
refusing would convert that possibility into a certain crash on a path nobody
has proven wrong, and the crash trail is the thing this whole epic exists to
produce. `set_lock_order_panics` makes that path reachable from a test, which is
why `a_release_build_stamps_an_inversion_and_carries_on` exists — a fail-open
path nobody has executed is a fail-open path nobody has checked.

The re-entrant half used to ride the same argument, and #1702 is that path
proven wrong in the field: `attention_tick` held `agents` across a call chain
that re-took `agents`, and four betas in a row shipped a hang whose whole
evidence was a human saying "it froze". Applied there, the fail-open sentence
reads "refusing would convert a certain hang into a certain crash" — which is a
trade with no upside, because the crash releases the registry and the hang does
not. **The requirement is therefore that a re-entrant `lock_safe` can never
block forever in a shipped build**, and `lock_safe` never reaches `inner.lock()`
on a lock this thread already holds.

`lock_safe` returns a guard, so the refusal is an unwind. Which unwind depends
on what the caller has:

| where the caller is | what happens |
| --- | --- |
| any build with the panic armed (debug, `cargo test`, the E2E lane) | panic naming both locks and both sites — the one mechanism that turns this class into a CI failure, so it is not softened by whether a budget frame happens to be installed |
| under a `read_budget` frame, panic off | `budget::unwind_to_frame` with the `BusyKind::Reentrant` `Busy`. The frame's owner already renders it: an MCP `isError`, a `partial` snapshot section, a command's empty degrade |
| no frame, panic off (a tick body under `tick_gate`'s `MutationScope`, an MCP mutate helper thread, a `run_blocking` body) | the same panic. `obs`'s hook writes a crash log naming both sites, and the unwind drops every guard — so the registry is **released**, which is the whole point |

Two consequences are paid for rather than waved at.

**It unwinds a SEALED frame** — the one narrowing of `lock-liveness.md` §4.1,
argued there.

**The crash log in the no-frame case is written while the outer lock is still
held**, because the panic hook runs before the unwind starts. That is the one
file write this module otherwise forbids on a lock-holding thread (§2, *What it
costs*). It is accepted here and nowhere else: a one-shot defect report on a
thread that is already leaving, whose alternative is a hang with no artifact at
all, and whose waiters are released by the unwind immediately after it.

**And the panic is bounded where it can repeat.** A cadenced tick that reaches a
re-entrant acquire reaches it every tick, and nothing prunes crash logs — so
supervision (`obs::TickSupervisor`, `TICK_PANIC_LIMIT`) stops calling a body
that has panicked three times running, and says so in a `tick-disabled`
breadcrumb. The transient case now recovers where it used to kill the thread;
the deterministic case degrades to what it did before, named.

### What it costs

Per acquisition, on top of everything Phase 0 and 2.1 already do: **two**
independent thread-local accesses — the check before the acquisition and the
push after it succeeds, each a TLS lookup plus a `RefCell` flag check — around
a scan of the held-lock stack (zero comparisons when the thread holds nothing,
which is the overwhelmingly common case, and at most 32 otherwise), plus one
store into a fixed array. Two rather than one because the check has to run
before the acquisition and the push only after it succeeds; folding them would
mean pushing a hold that may never happen. Per release there is **one** access
and a scan from the top, which finds its own entry on the first comparison
unless a guard was dropped out of order.

No allocation, no destructor at thread teardown, and nothing shared between
threads — which matters because the MCP server spawns one thread per request and
`lockwatch.rs`'s whole cost argument turns on the acquisition path touching only
this lock's own cache line. The stack is a fixed `[HeldEntry; 32]` rather than a
`Vec` for exactly that reason.

**A finding writes no file where it is made.** It stamps one slot on the lock —
four atomic stores — and the watchdog composes and writes it a second later,
beside the completed-hold reports it already drains. That is not tidiness: a
finding is made on a thread that is holding a lock, so a breadcrumb there would
land on the latency path of every waiter queued behind that hold. #1605 review
B1 established the rule for the release path (`TrackedGuard::drop` may not
allocate, format, take a global lock or make a syscall) and it applies here
unchanged — more so, because a real inversion on a hot path recurs every time
that path runs, where a slow hold at least has to be slow.

The one exception is the **debug panic**, which allocates and formats on the
spot. That is a build that is stopping.

## 3. Ranks are unique per field, and re-entrancy is decided by identity

The plan's sketch says "acquiring the SAME rank on the same thread" is the
re-entrant case. That is right about the intent and wrong as an implementation,
for a reason the test binary demonstrates several times per test: **it builds
more than one `OrchRegistry`**, so two live locks really do share a rank while
being two instances of one field rather than two peers. Refusing that nesting
would fail tests that have nothing wrong with them.

So the rule is split:

- every ranked FIELD gets a **distinct** rank. Three rows keep that true, and
  they are separate tests because two properties in one test means whichever
  assertion runs first masks the other: `l5_every_lockorder_rank_is_distinct`
  (no two consts share a value), `l5_every_lockorder_const_names_a_lock_that_still_exists`
  (a const still names a real lock), and `l5_every_lockorder_const_is_applied_to_its_field`
  — the one that makes "this field carries this rank" a claim a build can fail.
  That third row is not optional decoration: **removing a rank can only remove
  violations, never create one**, so a green suite is structurally incapable of
  noticing that a field has quietly gone unranked (#1610 review B1). It checks
  the converse too — no live lock may carry a rank `ALL` does not know — because
  a rank written inline at a construction site is invisible to the distinctness
  row, and two locks sharing a rank nest freely in *both* directions.
  **That converse direction reads the LIVE registry, so it can only see locks
  that have been constructed** (#1698 review residual): the registry's fields
  are all built by the time the test builds an `OrchRegistry`, but the
  dynamically-created ones are not — `usage_memo_cell` and the per-pane
  `delivery` locks come into existence when their group or pane does. A future
  ranked lock built lazily is therefore invisible to this row until the test
  forces its construction, and the fix when one is added is to force it there
  rather than to widen the scan;
- **equal ranks nest freely** — they can only be the same field twice;
- **re-entrancy is decided on the lock's identity**, the id `lockwatch` mints
  per lock, which is strictly what the epic's case is about and also catches it
  on *unranked* locks, where a rank comparison has nothing to say.

## 4. The table

`src-tauri/src/orchestration/mod.rs`, `pub mod lockorder`. Smaller is outer;
gaps are deliberate so a new lock can be slotted in without renumbering, because
a diff in which every line changed is one nobody can review for order.

| rank | lock | where the claim comes from |
| --- | --- | --- |
| 100 | `marker_io` | "taken FIRST and outermost — the set locks and `AUDIT_LOCK` are taken under it" |
| 200 | `group_file_io` | "taken FIRST and outermost — the `groups` lock and `AUDIT_LOCK` may be taken under it" |
| 300 | `queue_persist` | "this BEFORE `queues`, never the reverse" |
| 400 | `queues` | "`queues` -> `recovered_queue` -> `recovered_markers`" |
| 410 | `recovered_queue` | same claim, plus `archive_staged_overflow` |
| 420 | `recovered_markers` | same claim |
| 500 | `by_pty` | `session_for_pty` takes it, then `agents`, releasing this one first (stated from the code, not from that doc — see lock-liveness §6) |
| 510 | `agents` | same claim; and `delivered_prompts` below |
| 520 | `groups` | `group_file_io`'s claim puts it under that one |
| 600 | `delivered_prompts` | a caller resolving pty -> session takes `by_pty` then `agents` and releases both before this map; one holding a snapshot takes neither (stated from the code — see lock-liveness §6) |
| 610 | `delivered_notices` | "takes no other registry lock while held" |
| 700 | `agent_seq_persist` | "takes no other registry lock while held" |
| 800 | `tasks_lock` | `needs_you_lock`'s claim puts that one under it |
| 810 | `needs_you_lock` | "`tasks_lock` -> `needs_you_lock`, never the reverse" |
| 820 | `questions_lock` | "a leaf of its own" |
| 830 | `mailbox_lock` | "Lock order: nothing. It is taken alone" |
| 840 | `usage_lock` | "takes no other registry lock while held" except `AUDIT_LOCK` |
| 900 | `audit` (`AUDIT_LOCK`) | the innermost leaf; four of the file locks above name it as the one thing they take while held |

Two things the table does **not** claim.

**The order among the four file leaves (820/830/840 against 800) is arbitrary.**
Each of them is documented as taking nothing but `AUDIT_LOCK` and the app
handle, so nothing today nests any pair of them; the numbers had to be *some*
order, and an inversion report on one of those pairs would be a genuinely new
fact rather than a mistake in the table.

**`agent_seq_persist`'s claim has two halves and this enforces one.** "Takes no
other registry lock while held" is what a near-leaf rank enforces. "No caller
holds one when it calls in" is not a rank question, and nothing here checks it.

### What is deliberately unranked

About sixty-five registry fields, including `app`, `mq_state_lock`, the
attention cluster, the consent-flag sets, the intake maps and the per-pane
delivery locks. That is the plan's design rather than an unfinished edge: an
unranked lock may nest under anything, and it breadcrumbs `lock-rank-unranked`
the first time it does, so the rows the table is missing announce themselves
from the field instead of waiting to be noticed.

The ranked set is exactly the set someone has already written an ordering claim
about. A rank invented for a lock nobody has reasoned about would be a fact the
checker enforces and nobody has checked — which is the shape of defect this
whole epic is a response to.

## 5. What the derivation found

The method (#1610's brief): run the integration suite under the checker on CI
and assign ranks until it is silent. Every violation the checker reports is a
real, previously unwritten ordering fact, and each one is recorded here — with
whether the *table* was wrong or the *code* was.

**Zero violations, and that is the finding — dated to the round that actually
ran everything.** CI is a bare `cargo test --locked --workspace`, which stops at
the first failing test TARGET, so an intermediate round with any red in it left
targets unrun and is evidence about a subset rather than about the suite. The
claim belongs to the final round, which is green on all three platforms plus
E2E and therefore ran every target on each: in a debug build, where an inversion
or a re-entrant acquisition panics, it reported neither. (The run ids are in
#1698, which is the dated surface for them; a design note that cites one is a
note that goes stale on the next push.) So the eighteen ranks are not a guess
that happened to survive: they are the thirteen documented claims, and every
acquisition order the suite exercises agrees with them. That green suite *is*
the plan's L5c row.

That is a stronger result than the method expected, and it is worth saying why
it is plausible rather than suspicious. Most of the thirteen claims are *leaf*
claims — "takes no other registry lock while held" — and a leaf claim is
satisfied by a lock that nests under things and takes nothing, which is what the
ranks at 600-900 encode. The two claims with real nesting in them
(`queues` -> `recovered_queue` -> `recovered_markers`, and
`tasks_lock` -> `needs_you_lock`) name the direction the code already takes, and
the checker agrees with both. The pairs that would have been *interesting* —
`groups` against `agents`, `by_pty` against either — turn out never to nest at
all on the paths the suite runs: every site takes one, uses it and drops it
before reaching for the next — `session_for_pty` included, whose two
acquisitions are both statement temporaries. (That function's doc used to SAY
so; #1702 replaced the wording, because a promise about one body's internal
order is worth nothing to a caller that is already holding the outer lock. The
behaviour it described is unchanged, which is why the rank is.) The rank is
there for the site that stops doing that.

**What the derivation did find was a blind GUARD, not a wrong rank.**
`every_registry_lock_is_constructed_with_a_name` (`src-tauri/tests/selfwatch.rs`)
matched the literal `TrackedMutex::new(`. Seventeen of the eighteen ranked
fields are constructed in the scanned file (the eighteenth, `queues`, is built
in `crates/loomux-engine/src/queuestate.rs` through `QueueMap::new_ranked`), and
moving those seventeen to `new_ranked` dropped its count from 85 to 68 while
every one of them still passed a literal name — a guard blind to a fifth of the struct, which only its own vacuity floor
could say. It now classifies what FOLLOWS every `TrackedMutex::new` occurrence
and default-denies a constructor it has not been taught, with the vacuity
control per constructor rather than on the total: a single total is satisfied by
one constructor going to zero while the other absorbs it, which is precisely
what happened here.

The other two rounds changed no rank either — a platform-dependent test needle (a
recorded `file!()` is backslashed on Windows, so an `"orchestration/mod.rs"`
needle passed on two platforms and failed on the third), and the self-review that
moved every finding off the acquiring thread (§2, *What it costs*).

**The next rows will come from the field, not from here.** Sixty-five fields are
unranked; the first time each nests under anything, the watchdog writes a
`lock-rank-unranked` line naming it, the rank it nested under, and both sites.
That is the channel this section fills from next.

## 6. What this does not do

- **It does not remove a lock.** Phase 3b (#1611) collapses
  `groups`/`agents`/`by_token`/`by_pty` into one `Core`, at which point three of
  the ranks above become one. 3a lands first and alone so the registry moves
  with its order already declared.
- **It does not check the unranked majority.** By design (§4). The breadcrumb is
  what converges the table.
- **It does not see a path the process never takes.** A rank checker is a
  runtime instrument: it reports the orders that actually happened. The suite is
  what exercises them, and the release-mode breadcrumb is what covers everything
  the suite does not — which is the same bound the plan states for L5c.
- **It does not stop an INVERSION deadlock.** In release it reports one and gets
  out of the way (§2.1). What it removes there is the case where a hang left no
  evidence at all. It *does* stop a re-entrant one, in every build — that is
  #1702, and the difference between the two is §2.1's whole subject.

  **Why the earlier version of this bullet was half wrong.** 3a shipped one
  fail-open decision, taken once, at one switch, and applied to whatever
  verdict happened to arrive there. The sentence justifying it — *"refusing
  would convert a possible hang into a certain crash on a path nobody has
  proven wrong"* — is a good argument, and every clause of it is a claim about
  an **inversion**: *possible* (it needs a second thread), and *nobody has
  proven wrong* (no field report named one). Neither clause survives being
  carried over to a re-entrant acquire, which is certain rather than possible
  and which #1702 had already proven wrong in the field. The defect was not the
  argument; it was that one switch executed it for two kinds while only one had
  been argued. So the fail-open decision is now argued **per kind** and executed
  per kind — that is what §2's table has a kind column for, and what makes the
  two release cells legitimately different rather than inconsistent.
- **It does not make a refused re-entrant caller's work happen.** The refusal
  buys liveness, not correctness: the tick that was going to deadlock now
  panics, so its pass produces nothing and its badges hold their last value
  until the underlying re-entrancy is fixed (for `attention_tick`, that is
  #1702's own point fix). A registry that answers is strictly better than one
  that does not, and it is not the same thing as a registry that is right.
