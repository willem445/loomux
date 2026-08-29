# PTY input path: locks, threads and ordering (#719)

The companion to [pty-output-coalescing.md](pty-output-coalescing.md). That
note is about how many messages the *output* direction costs the GUI thread;
this one is about where the *input* direction is allowed to block.

## The defect

`write_pty` was a synchronous Tauri command whose body was:

```rust
state.note_user_input(id, &data, human);
let mut ptys = state.ptys.lock_safe();          // the GLOBAL map lock
let pty = ptys.get_mut(&id).ok_or("pty not found")?;
pty.writer.write_all(data.as_bytes())           // ...held across a blocking write
```

Two properties of that combine badly.

**`write_all` into ConPTY's stdin pipe is unbounded.** The pipe is small. It
drains only as fast as the child reads it, and an agent CLI that is busy
thinking, or wedged, reads nothing at all. The call then does not return —
not "slowly", but *until the child gets around to it*.

**`ptys` is one lock for every pane.** `write_pty`, `resize_pty`, `change_dir`,
`get_output`, `size`, `live_ids`, the attention scan (#40, #717/#725) and pane
teardown all take it. Holding it across the write meant one agent's full stdin
pipe stopped every pty command in the app, on every pane. That is the
"one agent is busy → the whole window is sluggish" report.

A third, smaller one: a synchronous command runs on the webview main thread
(the #716/#724 finding — and Tauri polls an *async* command's future there too,
up to its first real await, so work placed before the `spawn_blocking` call
does not escape either). So the wedge was also on the thread that paints.

## The fix, and the three things it deliberately does not change

`PtyHandle`'s `writer` and `master` became `Arc<Mutex<..>>`. Every caller now
clones the handle out under the map lock and releases the map lock *before*
touching the pipe. The map lock's hold shrinks from "however long the child
takes" to a hashmap lookup and an `Arc` clone. `write_pty` and `change_dir`
additionally became `async` and hand their **whole** body to
`spawn_blocking` — including `note_user_input`, whose throttled
`phantom-input-gated` breadcrumb (#496 N1) is a file write that had no business
on the painting thread.

### 1. Ordering — no queue, so no reordering

A pane's own writes must never reorder: a bracketed paste's `ESC[201~`
terminator landing before its body wedges the target app in paste mode (#65).

That guarantee does **not** come from the backend. It comes from
`src/ptywrite.ts`, which keeps exactly one `write_pty` in flight per pane and
chains the next call on the previous promise. The backend's obligation is only
to keep resolving that promise when the bytes are actually out — which is why
`write_pty` awaits the blocking write instead of returning at hand-off. A
per-pane writer thread with a queue was the other candidate shape; it buys a
FIFO guarantee that the frontend chain already provides, and pays for it by
breaking the two properties below. (**That last sentence is the one #1607
narrows** — it is true of a queue that returns at hand-off and false of one that
replies on completion. See "719 revisited on isolation" at the end of this note;
every other claim in this section stands.)

What async *does* give up is the accidental mutual exclusion between
**different** commands that main-thread dispatch provided (the same one #716
called out for `gh`): two sync commands could never overlap, and now they can.
The complete list of what that touches, and why none of it is load-bearing:

| pair | before | now | why it is fine |
| --- | --- | --- | --- |
| `write_pty` vs `write_pty`, same pane | ordered | ordered | the frontend chain, not the backend, is what orders these — one invoke in flight per pane (#65) |
| `write_pty` vs `change_dir`, same pane | ordered by arrival | **ordered by arrival again (#1607)**; between #719 and #1607, either order | both now go to the same per-pane writer thread, which runs its queue in arrival order. While they went to a shared pool it was either order, and that was fine for the reason #719 gave: each write is still atomic under the pane's writer lock, so neither can land inside the other, and a human is steering the folder picker or typing, not both in one instant |
| `write_pty` vs `resize_pty` | ordered by arrival | either order | there is no contract between a keystroke and a geometry change, and a resize the pane disagrees with is re-fired by the next fit tick |
| anything, across panes | ordered | either order | panes share no input state |

### 2. Bounded memory — back pressure, not a bounded queue

"What happens to the queue when the pipe stays full?" has no good answer. A
bounded queue must either grow (unbounded memory) or drop (a truncated command
line, submitted by the Enter queued behind it). The answer here is that there
is no queue: a pane whose child has stopped reading simply stops accepting
chunks, the frontend's chain stops dispatching, and the unsent remainder waits
in the pane's own JS queue exactly as it does today. Nothing accumulates
backend-side; in-flight bytes stay bounded by `PTY_WRITE_CHUNK` (16 KiB) per
pane.

`PtyManager::write_bytes` — orchestration's own typing — is synchronous-
completion for the same reason plus a sharper one: its `Ok` has always meant
"these bytes went out", and `record_delivered_text` (#576) and
`deliver_prompt`'s echo-verified typing loop both read it that way. A queue
that returned early would silently turn both into claims about bytes that may
never be written.

What the fix costs instead is one parked thread per wedged pane, bounded by the
pane count — against, before, one frozen GUI thread for all of them. Which
thread that is changed in #1607 and the bound did not: it was a slot in the
shared blocking pool, and it is now the pane's own writer thread.

### 3. `note_user_input` runs before the write, not after

`note_user_input` stamps keystroke recency and the input-box occupancy counter
that the question gate, the stranded-text flush and the autonomous idle tick
read to answer "is a human mid-typing in this pane?" (#111, #171, #496, #518).
It stays *before* the write.

Recording first means the answer is "yes" for the entire window in which the
keystroke is in flight — including the pathological window this issue is about,
where that window is seconds long. Recording after would leave it reading
"nothing typed" for that whole time, and a delivery would paste over a line the
human has already committed to, which is the clobber #111 exists to prevent.
The opposite error — believing a human typed slightly before their bytes land —
only ever makes loomux hold *more*, the fail-safe direction every one of those
readers is written for.

## Why `resize_pty` stays synchronous

It gets the lock half of the fix (the ConPTY call is outside the map lock) and
not the thread half, on purpose. There are two reasons, and they are
independent — which matters, because the first one is an assumption.

### Reason 1 (ASSUMED — the docs are silent)

**The claim:** a resize is bounded local work and cannot park on the child the
way `write_all` can, so it cannot produce the unbounded stall above.

**What the reference actually says.** The `ResizePseudoConsole` page
([learn.microsoft.com/en-us/windows/console/resizepseudoconsole](https://learn.microsoft.com/en-us/windows/console/resizepseudoconsole))
is 162 words. In full, its description and remarks are:

> Resizes the internal buffers for a pseudoconsole to the given size.

> This function can resize the internal buffers in the pseudoconsole session to
> match the window/buffer size being used for display on the terminal end. This
> ensures that attached Command-Line Interface (CUI) applications using the
> Console Functions to communicate will have the correct dimensions returned in
> their calls.

It says nothing about blocking, synchrony, or the attached application. It does
not say the claim is true and it does not say it is false: **the docs are
silent**, so this is an assumption, not a citation. (The "repaints the whole
screen" half is not Microsoft's claim either — it is this repo's own observation
about the Win10 inbox conhost, the one CLAUDE.md constraint 1 and #63/#432 rest
on.)

**What supports it anyway, labelled as what it is.** Two observations, neither
of them proof:

- #432 exists because resizes are *expensive in bursts* — its whole design is
  debounce, drag holds and same-size skip. Nothing in it, or in #63, reports a
  resize that failed to **return**.
- `resize_pty` has been running synchronously on the webview thread for the
  life of the app, and #719 fingered `write_all` specifically. A resize that
  parked on a busy child would have produced the same freeze, attributed to a
  different call.

**What would falsify it, and what to do then.** A GUI freeze correlated with
resizing rather than typing. The fix at that point is a **sequence-guarded**
async resize — take an ordering ticket before the first await and skip a
superseded one — *not* a plain `async`, which would relocate the block and
reintroduce the reorder Reason 2 is about.

### Reason 2 (verifiable from this repo's code)

A synchronous command inherits
arrival ordering from the main thread's dispatch, which resizes need:
`shouldResizePty` (src/panefit.ts) suppresses only an *identically sized*
in-flight call, so two different sizes can be outstanding at once. Off-thread
they could land in either order and leave ConPTY at the older geometry with no
event to correct it. A few milliseconds of jank is the cheaper side of that
trade — and this reason stands whatever Reason 1 turns out to be.

## What Microsoft *does* document, and why it endorses this change

The ConPTY reference is silent on resize blocking, but it is explicit about
threading, in a warning that reads as a description of the pre-#719 bug
([Creating a Pseudoconsole session](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session)):

> To prevent race conditions and deadlocks, we highly recommend that each of the
> communication channels is serviced on a separate thread that maintains its own
> client buffer state and messaging queue inside your application. Servicing all
> of the pseudoconsole activities on the same thread may result in a deadlock
> where one of the communications buffers is filled and waiting for your action
> while you attempt to dispatch a blocking request on another channel.

The same page also states that the channels are serviced with **synchronous**
I/O ("These channels are processed by the pseudoconsole system using ReadFile
and WriteFile with synchronous I/O"), which is why the input write blocks at all.

Before #719 loomux serviced the output channel on its own reader thread but
dispatched the input channel's blocking write from the webview thread, under a
lock shared with every other pane. This change puts the input channel on its own
thread too, which is what the warning asks for — and #1607 makes it literal: the
input channel now has a thread of its own per pane, exactly as the output
channel has had since before #719.

## 719 revisited on isolation (#1607, epic #1600 Phase 2.3)

#719 declined a per-pane writer thread. #1607 built one. Both are right, because
they answer different questions — and the discipline this note owes its next
reader is to say exactly which sentence moved.

### What changed underneath, and why the old answer stopped being complete

#719's fix put the whole body on `tauri::async_runtime::spawn_blocking`. That
pool is shared with every converted `orch_*` command and has no configured size
anywhere in this tree, so it is tokio's default of 512. In the beta6 field
report (#1600 §1.2) a registry mutex was held for a very long time; every polled
`orch_*` tick then parked a blocking-pool thread on it, at roughly 2.5-5 per
second, and none returned. Minutes later the pool was full. From that instant
`write_pty`'s task could not be scheduled at all, its promise never resolved,
`src/ptywrite.ts`'s per-pane chain stopped dispatching, and **every pane at once
refused input** while the window kept painting.

Nothing about that is a lock-scope bug, which is what #719 was about. It is a
*shared bounded resource* bug: the app's most latency-critical path was queued
behind orchestration's polling for a resource neither of them owns. #719 never
considered the question because the resource is not in the performance model
(#1600 §2.1) — every invariant there is about the webview thread, and the
destination work is moved TO is ungoverned.

### The one sentence that is superseded, and it is half a sentence

§1: *"A per-pane writer thread with a queue was the other candidate shape; it
buys a FIFO guarantee that the frontend chain already provides, and pays for it
by breaking the two properties below."*

- **"it buys a FIFO guarantee the frontend chain already provides" — STANDS,
  unchanged.** The per-pane thread is still not where a pane's keystroke
  ordering comes from; `src/ptywrite.ts`'s one-invoke-in-flight chain is, and
  #1607 does not touch it. The thread's FIFO remains redundant for that purpose,
  and it is not why it was built.
- **"and pays for it by breaking the two properties below" — SUPERSEDED, and
  only for the shape #1607 ships.** That cost is real for a queue that *returns
  at hand-off*, which is the only shape #719 weighed. It does not apply to a
  queue whose job carries a **completion reply**: `WriterJob` holds a
  `tauri::async_runtime::channel(1)` used as a oneshot, and `write_pty` resolves
  on that reply, not on the enqueue.

Everything else in §1, §2 and §3 stands as written, and each is worth naming
because each is the thing a reviewer should check has not quietly moved:

- **§1's ordering guarantee** — still the frontend chain, still #65. The one
  row of §1's table that moves is `write_pty` vs `change_dir`, which is ordered
  by arrival *again* now that both go to one pane-owned queue. That is a
  property regained, not one traded.
- **§2's back pressure** — unchanged, and this is the load-bearing one. A pane
  whose child has stopped draining still stops resolving, so the frontend chain
  still stops dispatching and the unsent remainder still waits in the pane's own
  JS queue. Nothing accumulates backend-side. `tests/liveness.rs` L3b pins both
  halves: the healthy pane's write lands while the wedged one is parked, *and*
  the wedged one has neither reported completion nor put any bytes in the pipe.
- **§2's `write_bytes` carve-out** — unchanged and deliberately not routed
  through the writer. Its caller is an orchestration background thread, never
  the frontend's pool, so it was never what beta6 starved; and its `Ok` must go
  on meaning "*this thread* wrote the bytes" for `record_delivered_text` (#576)
  and the echo-verified typing loop.
- **§3's ordering** — `note_user_input` still runs before the write. It is the
  same body (`write_from_frontend`), now executed by the pane's writer thread
  rather than a pool thread; both callers share one implementation, so there is
  no second copy to drift.

### Why a thread per pane rather than a small dedicated pool

A pool of size *k* is the beta6 mechanism again with an arbitrary *k*: it is a
cliff at *k* wedged panes. Sizing *k* to the pane count IS a thread per pane,
plus bookkeeping. The alternatives that only move the cliff — raising
`max_blocking_threads`, a second runtime, a global `async_runtime::set` with a
bigger pool — were rejected for the same reason. A fire-and-forget queue was
rejected on #719's own grounds, which still hold.

The cost is one thread per open pane. That is bounded by the UI, it is the same
count of *parked* threads a wedged pane already cost, and it is what ConPTY's
own guidance asks for.

## Locking rules this establishes

- The writer lock and the master lock are **leaves**. Nothing takes the `ptys`
  map lock, the output-ring lock, or `neutral_gate_throttle` while holding
  either. Every acquisition goes map-lock → release → leaf-lock.
- Nothing blocking-on-a-peer may happen under the `ptys` map lock. The reader
  thread and the attention scan never touch the writer at all, so the paths
  #717/#725 shortened stay short.

## Tests

`src-tauri/tests/ptywrite.rs`. `register_gated_fake_for_test` registers a real
ConPTY pair whose writer parks inside `write` until the test releases it —
"the child stopped draining its stdin", deterministically, with no sleeps and
no real agent CLI. The tests then assert that while one pane is wedged, the
global map lock is free, another pane's write lands, and the human-input
signals are already readable; plus that a write still does not return until its
bytes are out, that a pane's own writes concatenate in order, and that a write
to a dead pane errors rather than panics.

`src-tauri/tests/liveness.rs` (#1607) covers the isolation half, which no lock
test can reach. **L3a** installs a tokio runtime with `max_blocking_threads(2)`
as the app's `tauri::async_runtime`, parks two tasks in it, and asserts a
frontend write still completes and its bytes land. Its setup assertion is the
control that makes the result mean anything, and it is the pre-#1607 path
verbatim: a `spawn_blocking` hand-off of `write_from_frontend` on that same
saturated pool, asserted NOT to complete. **L3b** wedges one pane through the
seam and asserts the other pane's write lands while the wedged one still reports
nothing and still has nothing in its pipe. Both use `register_fake_for_test` /
`register_gated_fake_for_test`, which register the pane's real writer thread —
so the harness drives the shipped path, not a test-only variant of it.
