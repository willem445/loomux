# Self-observability: what the app can say about its own hang

Status: implemented (#1601, Phase 0 of `doc/plans/responsiveness-root-cause.md`).

This note is about an instrument, not a feature. Nothing here bounds anything,
refuses anything, or changes a single user-visible behaviour. What it changes is
what exists on disk after the app stops answering.

## 1. Why it exists

`crates/loomux-engine/src/obs.rs` writes a crash log when the process dies. A
process wedged on a mutex **does not die**, so it writes nothing — and the total
evidence for three consecutive incidents (#1592, #1595, and the beta6 field
report) was a human saying "it froze", plus an agent reading tens of thousands
of lines of Rust to construct a story that fit. Each story was plausible,
partially right, and shipped as a remedy; each remedy relocated the stall to a
different scarce resource. The plan's §2.3 names this as the highest-leverage
gap in the system, and the shape of the missing artifact precisely:

> One breadcrumb reading *"`mq_state_lock` held 340 s by `queue_merge_with`, 87
> waiters, pool in-flight 512"* would have collapsed beta4, beta5 and beta6 into
> a single diagnosis.

That sentence is the specification. Everything below is what it takes to be able
to write that line.

## 2. The four things it records

| what | where | reported when |
| --- | --- | --- |
| who holds a lock, since when, from which call site, with how many waiters | `lockwatch::TrackedMutex` | a hold past 5 s, and again on release with the total |
| how deep the shared `spawn_blocking` pool is | `selfwatch::pool_enter`, one door in `blocking::spawn_counted` | the peak crosses 64 / 128 / 256 |
| whether the backend is being scheduled at all | `selfwatch::watchdog_stamp` | on divergence, once per episode |
| whether the webview thread is being scheduled at all | `liveness_stamp` ← `src/liveness.ts` | on divergence, once per episode |

All four are breadcrumbs (`obs::breadcrumb`) — a deliberate non-decision. The
plan's §3 Phase 0.2 says to use the existing mechanism, and a second log would
be a second thing to find, rotate, and remember exists.

## 3. The decisions worth arguing

### 3.1 A type swap, not a call-site change

`TrackedMutex::lock_safe` has the **same signature and the same poison-tolerant
semantics** as `obs::LockExt::lock_safe`, and the inherent method shadows the
trait one. So all 448 call sites in `orchestration/mod.rs` are untouched and the
migration is a change of field type. A change that touched 448 sites would be
unreviewable, and the review is the point — this is a change to the app's
hottest shared path.

What *did* change, and could not not: the eight function signatures that name a
lock's type by hand, two `static`s that became `OnceLock` getters (registering a
lock is not a `const` operation), and one `.or_default()` that had to become
explicit — a `TrackedMutex` has no `Default`, because **a lock with no name is a
lock a hold report cannot identify**, and the name is the first half of the
sentence in §1.

### 3.2 Two reporters, and they are not redundant

The guard's `Drop` reports a hold that exceeded the threshold, with the exact
total. The watchdog reports a hold that is **still in flight**.

Neither covers the other. `Drop` cannot report a hold that never ends — which is
the beta6 case, and the only one that matters most. The watchdog cannot report a
hold that began and ended between two of its looks. Together every hold past the
threshold is reported by one of the two, and neither depends on the other being
wired correctly.

### 3.3 The observer never takes the lock it is observing

The watchdog reads a hold's fields from another thread **without touching the
mutex**. An instrument that waited on the lock it is reporting would be the
first casualty of the hang it exists to describe.

That makes the fields a multi-word record read concurrently with its writer, so
they are published behind a generation counter in the seqlock shape: even =
free, odd = held; the holder writes the fields, then publishes the generation
with a `Release` store; a reader takes the generation, reads the fields, takes it
again, and discards the sample if it moved. The mutex itself serialises the
writers, so there is exactly one writer per generation and no CAS loop is needed.
`(lock id, generation)` then identifies one hold for the process's lifetime,
which is what lets the watchdog report a fifteen-minute hang **once** instead of
nine hundred times.

### 3.4 What acquiring and releasing cost

On top of the `Mutex::lock` that was already there — on acquire: two relaxed
read-modify-writes on the waiter count, three relaxed stores, one release
read-modify-write on the generation, and one monotonic clock read. On release:
one clock read, four relaxed loads, four relaxed stores and two release
read-modify-writes. **No allocation, no formatting, no global lock, no syscall,
and nothing that can block, on either** — the global registry is touched at
construction only, and every byte of every report is composed on the watchdog
thread.

The clock read is the only item above a few nanoseconds. A cheaper design was
available and rejected: stamping holds against the watchdog's 1 Hz tick would
cost one atomic load, and would floor every reported duration at a second — on
the one instrument whose entire job is to say how long something took.

**The release path did not always look like this, and the reasoning that got it
wrong is the part worth keeping.** It used to compose and write its own report —
a `format!`, the report ring's process-global mutex, and `create_dir_all` +
`metadata` + `open` + `write_all` — excused by the sentence "a hold that has
already lasted seconds is not a hot path".

That sentence is true of the *holder* and false of everyone else. `Drop::drop`
runs **before** the struct's fields drop, and the `MutexGuard` is a field, so all
of it executed with the reported mutex still locked. And a hold long enough to be
worth reporting is precisely the one with waiters queued behind it — so the file
write was not on the holder's latency path, it was on **every waiter's**, which
is the path this whole plan is about. `performance.md` X4's `mq_state_lock` makes
it concrete: a merge-queue tick whose `gh` call takes more than five seconds is
routine, and it would have appended a breadcrumb inside that lock's critical
section with every MCP request thread waiting behind it. The feedback ran the
wrong way — the worse the app behaved, the more IO inside contended sections.

So the release path now only *stamps*, in atomics, and
`lockwatch::drain_completed_holds` composes it on the watchdog thread. The repo
already held this line elsewhere: X6 credits `AUDIT_LOCK` for being "held only
for the open+write ... the JSON is formatted before the lock is taken". Found in
review (#1605 B1), and pinned by
`the_release_path_takes_no_global_lock_while_the_mutex_is_still_held`, which
makes the report ring unavailable and asks whether a release completes anyway —
because reading the code is what missed it the first time.

### 3.5 The pool is counted at the hand-off, through one door

`blocking::spawn_counted` is the only `tauri::async_runtime::spawn_blocking`
call in `src-tauri/src`, and `src-tauri/tests/selfwatch.rs` pins that.

Two decisions in that sentence. The ticket is taken **before** the hand-off and
moved into the task, so the depth counts work that is still *queued* as well as
work that is running — a counter incremented inside the closure would read 512
at saturation and go no higher, hiding the queue behind it, which is the number
the plan's §1.2 mechanism actually turns on. And there is **one door** rather
than eight wrapped sites, because a depth reading means something only if it is
complete: `in-flight 480` is a diagnosis, and `in-flight 480 plus however many
sites nobody wrapped` is not. Eight wrapped sites is a convention somebody has
to remember; one door is a property a scan can pin.

The watchdog samples the **peak** since its last look (`pool_take_peak`), not
the instant, so a crossing that happened between two looks cannot be missed by
sampling — only by the pool never actually filling.

### 3.6 The heartbeat must not depend on what it measures

beta5 and beta6 present identically — the window is up and the app does not work
— and are opposite failures. Telling them apart cost a release cycle.

So both halves stamp, and `selfwatch::liveness` is the pure verdict over the
two. Three things make it honest:

- **`liveness_stamp` is sync** (`performance.md` §4 X7). Delegated, it would
  stop running exactly when the pool is exhausted, and the heartbeat would
  report the webview stuck on the one occasion it is the only healthy half left.
- **The watchdog reports its own scheduling lag**, and that lag is part of
  "backend fresh". This function is called from the watchdog tick, microseconds
  after that tick stamped, so a freshness test on the stamp alone would answer
  `Ok` from inside a starved backend every time.
- **A hidden window is "no evidence", not "stuck".** The platform throttles a
  hidden window's timers, so a stale stamp from one says nothing; `GuiHidden` is
  a distinct verdict and writes no breadcrumb. Calling it `GuiStuck` would fire
  the alarm every time the human minimizes the app, which is how an instrument
  stops being read. The residual is stated at the variant: a GUI genuinely
  wedged while hidden reads as `GuiHidden`, and what bounds it is that the
  human's next interaction un-hides the window and the very next tick reads
  `GuiStuck` for real.

### 3.7 std, not `parking_lot` — for now

`parking_lot` is already in the linked graph via `tao` and carries no
`getrandom` edge, so CLAUDE.md constraint 2 would not have refused it, and its
`try_lock_for` is the natural vehicle for Phase 2.1's bounded acquisition.

It is still not adopted here. This change's whole claim is that it alters no
behaviour, and swapping the app's primary synchronisation primitive is not that.
`TrackedGuard` is the app's own type and `std::sync::MutexGuard` is never
exposed, so Phase 2.1 can swap the inner primitive **invisibly** — the
dependency belongs to the change that needs it, where its argument can be made
against a real requirement rather than against a convenience.

## 4. What this does not do

It does not bound a wait, refuse an acquisition, or isolate the pty write path
from the shared pool. Those are Phases 1 and 2 of the plan and each is a
behaviour change with its own argument to make. (The pty half is no longer
pending: #1607 landed Phase 2.3 in parallel with this, putting the input path
on a thread per pane — `doc/design/pty-input-path.md` § "719 revisited on
isolation". Bounding a wait and refusing an acquisition are still ahead.) `#1601` exists so that those
changes are chosen against evidence — which is the one thing the last three
attempts did not have.

(It does not single-flight a poll either, and that one is no longer a Phase 1
job: #1602/#1604 landed it in parallel with this, and `performance.md` INV-4
carries the rule. Named here because this list read as the complete set of what
is still missing, and a list like that goes stale the moment a sibling merges.)

## 5. Reading the output

Breadcrumbs land in `<data root>/logs/breadcrumbs.log`, one line of
`stamp event detail`:

| event | means |
| --- | --- |
| `watchdog-start` | the watchdog thread came up; carries the threshold and the live lock count |
| `lock-slow` | a lock has been held past the threshold and **still is** |
| `lock-freed` | a lock that was held past the threshold has been released; carries the total |
| `pool-depth` | the blocking pool's peak crossed 64 / 128 / 256 |
| `live-gui-stuck` | the backend is ticking and the webview has stopped stamping |
| `live-backend-stuck` | the webview is stamping and the backend is not being scheduled |
| `live-both-stuck` | neither half is answering |

A `lock-slow` with no matching `lock-freed` is a hold that never ended. That is
the line the last three releases were missing.
