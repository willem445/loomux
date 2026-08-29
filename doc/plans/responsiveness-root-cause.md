# Plan: stop the beta3→beta6 responsiveness regressions at the root

Status: accepted — in progress (#1600). Written after the beta6 field
report (orchestrator loses the MCP, then every pane stops accepting input
while the window keeps painting).

This note deliberately does **not** open with a fix. Three betas in a row have
shipped a fix derived from reading code, and each one relocated the symptom
instead of removing it. The first section is the mechanism; the second is why
the existing guard system could not see it; the plan follows from both.

---

## 1. What is actually happening

### 1.1 The symptom chain, beta4 → beta6

| release | symptom | shipped remedy |
| --- | --- | --- |
| beta4 | hangs then aborts within seconds of opening on a large corpus (#1592) | boot listing path made non-blocking |
| beta5 | window permanently unresponsive 1-2 min after launch, `crash_log=none` (#1595) | five polled `orch_*` commands converted sync → async |
| beta6 | window **responsive**, MCP unreachable, **every** pane refuses input | — (this report) |

These are not three bugs. They are one defect being pushed from thread to
thread. Each fix moved the *victim* of an unbounded wait; none of them removed
or bounded the wait.

### 1.2 The mechanism

Established from the code, not inferred:

- `OrchRegistry` carries **17 `Mutex` fields**; `orchestration/mod.rs` declares
  90 mutexes across 47,035 lines and contains **407 `lock_safe()` call sites**.
- `lock_safe()` is `self.lock().unwrap_or_else(|e| e.into_inner())` — poison
  tolerant and **unbounded**. There are **zero** `try_lock` and zero timed
  acquisitions in the entire orchestration module.
- Those same mutexes are taken by background threads: the idle reaper, the
  agent watchdog, the unified `gh` poller, the merge-queue driver, and
  `note_agent_activity` on the pty output path.
- The MCP server (`orchestration/mcp.rs`) is `tiny_http` with
  `std::thread::spawn` per request. Every tool call resolves its token and then
  takes registry locks, unbounded, on that thread.
- `write_pty` (`pty.rs:1736`) and every converted `orch_*` command hand their
  bodies to the **same** `tauri::async_runtime::spawn_blocking` pool. Nothing
  in the tree configures `max_blocking_threads`, so it is tokio's default:
  **512**.
- `src/ptywrite.ts` keeps exactly **one `write_pty` in flight per pane** and
  chains the next call on the previous promise (#65 ordering guarantee). A
  `write_pty` that never resolves stops that pane accepting input **forever**.

Put those together and beta6 falls out, in the order the report gives it:

1. Something acquires a registry mutex and holds it for a very long time or
   forever.
2. **MCP dies first.** Every `tools/call` thread parks on that mutex. The
   orchestrator's next tool call never returns. This is immediate.
3. The group view keeps polling — 5 commands per open group view every 2 s,
   plus one per group-bound tab every 4 s. Post-#1595 each of those is `async`,
   so each tick parks a **blocking-pool thread** on the same mutex instead of
   the webview thread. They accumulate; none ever returns.
4. **The pool exhausts.** At roughly 2.5–5 parked threads per second, 512 is
   reached in minutes. From that moment `write_pty`'s `spawn_blocking` can no
   longer be scheduled, its promise never resolves, and the frontend's per-pane
   chain stops dispatching. **Every pane, at once, stops accepting input.**
5. The webview thread is untouched, so the window paints, the panes render
   backend output, and the UI feels alive. Exactly as reported.

#1595 named this outcome and dismissed it:

> Saturating 512 would need a single hold of the order of minutes — which is
> the *other* bug, and one this audit found no path to.

The audit found no path because of what it searched for: a two-lock ordering
inversion between `agents` and `groups`, and file IO or a subprocess held
inside an `agents` guard. It did not and could not cover a long hold on any of
the other 15 registry mutexes, a re-entrant acquisition of the same mutex
(which self-deadlocks permanently, with no cycle for an inversion search to
find), or a lock held across a pty write into a stdin pipe whose child has
stopped reading — which `doc/design/pty-input-path.md` documents as **unbounded
by nature**.

The audit's conclusion — *"nobody holds the lock pathologically long"* — was
the load-bearing premise of beta6's fix, and beta6 is the counterexample.

**What is not yet established:** *which* hold. That is the point of §3 Phase 0,
and it is deliberately not guessed at here. Guessing is what produced beta5 and
beta6.

---

## 2. Why the guard system did not catch this

This repo has an unusually strong invariant culture: `doc/design/performance.md`
carries six numbered invariants, two enforcement test suites, an
argued-exception table and a debt register. It did not help. Three reasons —
and these, not the mutex, are the root cause of the *regression pattern*.

### 2.1 The performance model has exactly one scarce resource in it

`performance.md` §1 opens: *"One thread — the webview/GUI thread — services
**all** of…"*. Every invariant is written about that thread. INV-1 is about
command dispatch onto it, INV-2 about process spawns on it, INV-3/4 about what
reaches it.

So the model has no name for, and no rule about:

- the **shared `spawn_blocking` pool** — a bounded resource of 512, shared by
  every converted command *and* by `write_pty`, the app's most latency-critical
  path;
- the **MCP request threads**, which are the orchestrator's only channel and
  are governed by nothing;
- **lock acquisition time** as distinct from lock hold time. INV-5 bounds what
  a holder does. Nothing anywhere bounds what a waiter pays.

Every remedy the invariants prescribe is *"move it off the webview thread."*
The destination is ungoverned. beta5 → beta6 is that sentence, in one release.

### 2.2 The guards are shape scans; every one of these bugs is a liveness bug

`perf_dispatch.rs` reads source and asserts structure. Its newest guard,
`no_command_on_a_fixed_cadence_poll_path_is_synchronous`, is well built — and
it **would pass on beta6**, because beta6's poll commands are all correctly
async. It pins the shape of the previous incident.

That is the tail-chasing signature: each round adds a guard describing the last
failure exactly, and none of them observes the property the user actually cares
about — *does the app still accept input after running for a while under load*.
Four hangs (#1564, #1592, #1595, beta6) have now shipped past a growing wall of
source scans.

### 2.3 A hang produces no evidence, so every diagnosis is prose

`crates/loomux-engine/src/obs.rs` installs a panic hook and writes crash logs.
There is **no hang, deadlock, lock-hold or thread-pool instrumentation anywhere
in the tree**. `watchdog_tick` watches *agents* for stalls; nothing watches the
app's own threads.

The consequence is in the #1595 body itself: `crash_log=none`. When beta5
froze, the total evidence was a human saying "it froze" plus an agent reading
47,000 lines of Rust to construct a story that fit. The story was plausible,
partially right, and shipped as a remedy. beta6 is what the untested half of
that story did.

**This is the highest-leverage gap in the system.** One breadcrumb reading
*"`mq_state_lock` held 340 s by `queue_merge_with`, 87 waiters, pool in-flight
512"* would have collapsed beta4, beta5 and beta6 into a single diagnosis.

### 2.4 The batch size destroys the only integration signal there is

beta3 → beta4 was **40 commits**, including a cargo package rename, a new
resume route, a backend session-id learning change, and an 801-line change to
`orchestration/mod.rs`. beta4 → beta5 and beta5 → beta6 were 2 commits each,
both firefighting.

The human is the only integration test in this system. A 40-commit beta gives
that test no bisect signal at all.

---

## 3. The plan

Ordered by leverage. Phase 0 gates everything else: **no further responsiveness
fix ships on a hypothesis while the app cannot report what it was doing when it
stopped.**

### Phase 0 — Make the failure state observable (blocks all other phases)

**0.1 Instrumented locks.** Introduce `TrackedMutex<T>` in `loomux-engine`
exposing the same `lock_safe()` signature, so the 407 call sites are unchanged
and the migration is a type swap on the 17 registry fields. On acquire it
records `(thread id, call site via #[track_caller], acquired_at)`; the guard
clears it on drop. Waiters register before blocking.

**0.2 A self-watchdog thread.** One thread, 1 s cadence, off every hot path. It
breadcrumbs when any tracked lock has been held past a threshold (start at 5 s),
naming the holder's call site, the hold duration and the waiter count — and
again on release, with the total.

**0.3 Blocking-pool depth.** A counter around every `spawn_blocking` hand-off
(`blocking.rs::run_blocking`, `orchestration::run_blocking`, and the four raw
sites). The watchdog breadcrumbs when in-flight crosses 64 / 128 / 256. A
report that reads "in-flight 512" is a diagnosis rather than a mystery.

**0.4 A liveness heartbeat.** The watchdog stamps a timestamp each tick; the
webview stamps one each frame. Divergence separates "the GUI thread is stuck"
from "the backend is stuck" — the exact distinction that cost a whole release
cycle between beta5 and beta6.

Cost: small, self-contained, no new dependency, no product behaviour change.
Value: every future report arrives with its mechanism attached.

### Phase 1 — Remove the poll path's contention entirely

The polled commands do not need the live registry. They need a *view* of it.

Replace per-command acquisition with a **single snapshot publisher**: one owned
thread computes the group-view and tab-strip payload on a cadence, under the
registry locks, and publishes it into an `RwLock<Arc<Snapshot>>` (or
`ArcSwap`). Every polled `orch_*` read becomes an `Arc` clone — no contention,
no possible wait.

This is the structural version of what beta6 attempted, and it differs in kind:

- it holds regardless of whether a command is sync or async, `cheap` or
  expensive — the classification problem that produced #752, #743 and #1595
  stops applying to reads altogether;
- one snapshot serves every group-bound tab, so the tab-strip's per-tab fan-out
  (the doubled poll site #1595 found late) collapses to O(1);
- a stuck registry then yields a *stale panel* — visible, bounded, recoverable,
  INV-6 applied to the registry — instead of an unbounded queue of parked
  threads.

### Phase 2 — Bound every acquisition a human or an agent waits behind

**2.1 No unbounded `lock()` on a waited path.** Add `lock_timeout(Duration)` to
the tracked mutex. On timeout: return a typed `Busy` error and breadcrumb.
Polled reads render stale-with-badge; MCP returns a retryable JSON-RPC error
instead of hanging the orchestrator's turn. A bounded wait converts a deadlock
into a degraded surface plus evidence.

`parking_lot::Mutex::try_lock_for` is the natural vehicle; it must be checked
against CLAUDE.md constraint 2 (`getrandom`) before adoption, with a `try_lock`
+ backoff loop on `std` as the fallback if it fails that check.

**2.2 Single-flight the poll path.** A tick that finds its own previous call
still outstanding skips instead of issuing another. This alone makes pool
exhaustion **unreachable from the poll path**, whatever the hold does — and it
is a small change in `src/orchestration.ts` plus the two poll sites.

**2.3 Isolate `write_pty` from the shared pool.** The app's input path must not
compete for threads with orchestration polling. Either a dedicated small pool
for pty writes, or the per-pane writer thread #719 considered and declined.
#719 declined it because the frontend chain already provides FIFO — that
argument is about *ordering*, and says nothing about *isolation*, which is what
beta6 needed. Re-decide it on the isolation question.

### Phase 3 — Reduce the lock surface so an audit can be complete

17 mutexes on one struct with **no declared lock order** — `resolve_token`'s own
comment says so: *"locking them together would pin a lock order no other call
site promises to respect."* No audit over that surface can be exhaustive, which
is why #1595's was not, and why the next one will not be either.

Direction — incremental, and behind Phase 1, which removes the read pressure
first:

- collapse the core maps (`groups`, `agents`, `by_token`, `by_pty`) behind one
  `RwLock<RegistryState>` with a single documented, tested order; the data is
  already effectively single-writer / many-reader;
- or move the registry to a message-passing owner thread, which makes the
  ordering question disappear rather than documenting it.

`orchestration/mod.rs` at 47,035 lines is why every audit here is partial. The
engine extraction (#888) is the vehicle already in flight; the registry's
locking core is a good next batch for it.

### Phase 4 — Change what a beta is allowed to contain

**4.1 A soak lane.** The E2E harness (Playwright over WebView2 CDP) already
exists and already runs on CI. Add one spec that is a *liveness* test rather
than a shape test: launch against a large synthetic corpus, open several
group-bound tabs, idle 10 minutes, then assert a keystroke reaches a pane and
an MCP `ping` answers within a bound. This is the single test that would have
caught all four hangs. It needs no live agent CLI — a fake child and a
synthetic corpus suffice, so CLAUDE.md constraint 3 is untouched.

**4.2 Small betas while the class is open.** A 40-commit beta wastes the only
useful output the human integration test produces: *which change did it*. Until
the soak lane is green and Phase 0 has shipped, betas should be small enough to
bisect from a single field report.

**4.3 Freeze orchestration-core feature work until Phase 0 + 1 land.** 173 open
issues is a lot of pull toward the next feature. The last three betas produced
no net user-visible progress; the cost of the freeze has already been paid —
just in a form that also cost three broken installs.

---

## 4. What this plan deliberately does not do

- **It does not name the beta6 culprit hold.** That is Phase 0's output, not an
  input. Every previous round began by naming a culprit from a reading, and
  each was partially right in a way that shipped.
- **It does not add another source-scan guard.** The scans are good at what they
  do, and would not have caught any of the last three. §2.2.
- **It does not add a seventh invariant to the current model.** The model needs
  its missing resources first (§2.1) — the blocking pool, the MCP threads, and
  acquisition time. Invariants over a model that omits the scarce resource are
  exactly what produced a *correct* classification of five commands as `cheap`
  that then froze the app.

---

## 5. Sequencing

| step | scope | gates |
| --- | --- | --- |
| Phase 0 | tracked locks, self-watchdog, pool depth, heartbeat | — |
| beta7 | Phase 0 only, nothing else | ships to get evidence, not a fix |
| Phase 2.2 + 2.3 | single-flight polls, isolate `write_pty` | cheap, independent of the diagnosis |
| Phase 1 | snapshot publisher for polled reads | after beta7 evidence confirms the holder |
| Phase 2.1 | bounded acquisition + `Busy` degradation | after Phase 1 |
| Phase 4.1 | soak lane in E2E | parallel, any time |
| Phase 3 | registry lock consolidation | after Phase 1 |

The first shipped artifact of this plan is a beta that fixes nothing and
reports everything. That is the step the last three betas skipped.

---

## 6. Measurement note (added when this plan was committed, #1601)

Everything above is the epic body (#1600) verbatim, and it is kept that way: it
is the argument that was accepted, and rewriting an accepted argument in place
is how a document stops being checkable. What is corrected here instead are its
**numbers**, because a figure that is wrong for the tree the file lives in is a
false claim on a permanent surface, however true it was where it was measured.

§1.2's figures were taken on `src-tauri/src/orchestration/mod.rs` as it stood
at this work's branch point (`88818695`, blob `b280c2f8`) — about 8,000 lines
behind the `main` this plan lands on (blob `005d2e5d`). Both columns below are
measured, not derived:

| §1.2 says | at blob `b280c2f8` | at blob `005d2e5d` |
| --- | --- | --- |
| 47,035 lines | 47,035 — exact | **54,784** |
| 407 `lock_safe()` call sites | 407 — exact | **448** |
| 90 mutexes declared in the module | 92 `Mutex<` occurrences | **99** |
| `OrchRegistry` carries **17** `Mutex` fields | **75** — never 17 | **82** (66 plain, 16 behind an `Arc`) |
| zero `try_lock`, zero timed acquisitions | 0 — exact | 0 |

```sh
B=005d2e5dadfb05b539bad32da39d225c1d2fbe1e   # or b280c2f8… for the left column
git cat-file blob $B | wc -l
git cat-file blob $B | grep -c 'lock_safe()'
git cat-file blob $B | grep -o 'Mutex<' | wc -l
git cat-file blob $B | grep -c 'try_lock'
```

Three of the five are exact or near-exact for the tree they were taken on, and
went stale only by being written down before a rebase. **The fourth was never
right**, and it is the one that decided what Phase 0 built: 17 is close to the
count of `Arc<Mutex<..>>` fields alone (16 today), and taking it at face value
would have migrated a fifth of the registry — leaving 65 locks invisible, which
is exactly the blindness §1.2 blames for the previous audit missing this class
("a long hold on any of the other 15 registry mutexes"). Phase 0.1 therefore
migrated all 82, plus `AUDIT_LOCK`, the per-pane delivery lock, the per-group
usage-memo cell, and the engine-side `QueueMap` / `DrainerRegistry` /
gh-capture-backlog locks. `src-tauri/tests/selfwatch.rs` pins that a registry
field cannot go back to a plain `Mutex`, so this figure cannot silently drift
again.

None of this weakens the plan's argument. The mechanism in §1.2 does not depend
on how many locks there are — one unbounded hold is enough — and a larger lock
surface makes §3's case for Phase 0 and §4's refusal to name a culprit stronger,
not weaker.

**Each figure is dated to a BLOB rather than to a commit.** A rebase invalidates
every commit SHA a document could cite while leaving `git rev-parse
HEAD:src-tauri/src/orchestration/mod.rs` checkable; when that blob moves, the
anchor is what says the right-hand column needs re-measuring.
