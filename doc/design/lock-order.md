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

| what the checker sees | what it does |
| --- | --- |
| this exact lock is already on this thread's stack | **re-entrant**: `Err(Busy)` with `BusyKind::Reentrant` from `lock_within`; a debug panic from `lock_safe` |
| the rank being taken is *strictly below* the innermost rank held | **inversion**: debug panic naming both locks and both sites; release breadcrumb `lock-order-violation`, then acquire anyway |
| equal ranks | allowed — see §3 |
| the lock is unranked and something is held | breadcrumb `lock-rank-unranked`, once per lock; not an error |
| anything else | nothing |

The check is per-thread and **needs no second thread to fire**. That is the
whole reason it finds what a soak test cannot: it does not need the race to
happen, only the order that permits it. One test process taking `groups` then
`agents` once is enough to say the ordering fact exists.

**Both locks and both sites, in both surfaces.** A report naming only the lock
being taken sends the next reader back into a 54,000-line module to guess what
was already held, which is the state #1600 §2.3 describes. So the panic and the
breadcrumb both carry the acquiring lock, its rank and its `#[track_caller]`
site, and the same three for the hold it collided with.

### Fail open in release, and why that is not timidity

A shipped build breadcrumbs and then does exactly what it would have done
anyway. The deadlock risk is already in the shipped binary; refusing the
acquisition would convert a *possible* hang into a *certain* crash on a path
nobody has proven wrong, and the crash trail is the thing this whole epic exists
to produce. `set_lock_order_panics` makes that path reachable from a test, which
is why `a_release_build_breadcrumbs_the_violation_and_carries_on` exists — a
fail-open path nobody has executed is a fail-open path nobody has checked.

### What it costs

Per acquisition, on top of everything Phase 0 and 2.1 already do: one
thread-local borrow, a scan of the held-lock stack (zero comparisons when the
thread holds nothing, which is the overwhelmingly common case, and at most 32
otherwise), and one store into a fixed array. Per release: one thread-local
borrow and a scan from the top, which finds its own entry on the first
comparison unless a guard was dropped out of order.

No allocation, no destructor at thread teardown, and nothing shared between
threads — which matters because the MCP server spawns one thread per request and
`lockwatch.rs`'s whole cost argument turns on the acquisition path touching only
this lock's own cache line. The stack is a fixed `[HeldEntry; 32]` rather than a
`Vec` for exactly that reason.

Two paths *do* write a file from the acquiring thread, and both are bounded:
a violation report (once per defect — a build that hits it often has a deadlock
to fix first) and an unranked lock's first nesting (once per lock, and at most
128 per process, because the test binary builds registries in the hundreds).

## 3. Ranks are unique per field, and re-entrancy is decided by identity

The plan's sketch says "acquiring the SAME rank on the same thread" is the
re-entrant case. That is right about the intent and wrong as an implementation,
for a reason the test binary demonstrates several times per test: **it builds
more than one `OrchRegistry`**, so two live locks really do share a rank while
being two instances of one field rather than two peers. Refusing that nesting
would fail tests that have nothing wrong with them.

So the rule is split:

- every ranked FIELD gets a **distinct** rank (`lockorder::ALL` plus
  `l5_every_rank_is_distinct_and_names_a_lock_that_still_exists` keep that true);
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
| 500 | `by_pty` | "`by_pty` is taken and RELEASED before `agents`" |
| 510 | `agents` | same claim; and `delivered_prompts` below |
| 520 | `groups` | `group_file_io`'s claim puts it under that one |
| 600 | `delivered_prompts` | "`agents` is taken and RELEASED before this one" |
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

<!-- DERIVATION-LOG -->

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
- **It does not stop a deadlock.** In release it reports one and gets out of the
  way (§2). What it removes is the case where a hang left no evidence at all.
