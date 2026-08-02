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
breaking the two properties below.

What async *does* give up is the accidental mutual exclusion between
**different** commands that main-thread dispatch provided (the same one #716
called out for `gh`): two sync commands could never overlap, and now they can.
The complete list of what that touches, and why none of it is load-bearing:

| pair | before | now | why it is fine |
| --- | --- | --- | --- |
| `write_pty` vs `write_pty`, same pane | ordered | ordered | the frontend chain, not the backend, is what orders these — one invoke in flight per pane (#65) |
| `write_pty` vs `change_dir`, same pane | ordered by arrival | either order | each write is still atomic under the pane's writer lock, so neither can land inside the other; a human is steering the folder picker or typing, not both in one instant |
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

What the fix costs instead is one parked blocking-pool thread per wedged pane,
bounded by the pane count — against, before, one frozen GUI thread for all of
them.

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
thread too, which is what the warning asks for.

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
