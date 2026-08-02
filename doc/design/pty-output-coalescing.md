# PTY output coalescing (#712)

## The constraint this design is built around

**A `pty-output` event is not a cheap message. It is a JavaScript
compilation on the GUI thread.**

That is a property of Tauri's transport, not of loomux, and it is worth
spelling out because every instinct about "just send the data" is wrong
here. Following `Emitter::emit` down (tauri 2.11.5):

- `Webview::emit_js` calls `self.eval(event::emit_js_script(..))`.
- `emit_js_script` `format!`s a **new JS source string per event**, with the
  payload inlined as a literal:
  `(function () { const fn = window['…']; fn && fn({event: 'pty-output', payload: {"id":7,"data":"<base64>"}}, [1]) })()`.
- Called from a worker thread, `eval_script` goes through
  `send_user_message` → `proxy.send_event(..)` (tauri-runtime-wry 2.11.4):
  one event-loop wakeup on the GUI thread, which then calls
  `evaluate_script`, and the webview parses and runs that one-shot script.

`tauri::ipc::Channel` is not an alternative: below its direct-execute
threshold it evals too. **The transport is fixed; the only lever is how many
messages we send.**

The thread that pays is the one that also services keyboard input and
paints. So an unbounded event rate does not degrade gracefully into "output
appears a bit later" — it degrades into the whole window going sluggish.

## What went wrong

Before this, `spawn_pty`'s reader thread emitted once per `read()` return.
`read()` returns whatever ConPTY has buffered at that instant, so the event
rate was set by the child's write pattern — an agent CLI redrawing a status
line or streaming tokens emits a long train of small chunks — with nothing
bounding it, and multiplied by the number of live panes.

The reported shape (#712) fits exactly: with five or six busy agent panes the
whole app went sluggish (typing *and* scrolling) while total CPU sat at
15-30% — one saturated thread on a many-core box, not a saturated machine.
The first hypothesis in the issue was an input-path problem, because typing
was what the human noticed first; the refinement that *scrolling lagged too*
is what re-ranked it, because a shared saturated thread explains both and an
input-path defect explains only one.

## The policy

`ptyout.rs` sits between the reader thread and the emit. It is a
**leading-edge throttle** with a hard batch cap:

- **A chunk arriving into a pane that has been quiet for at least one window
  is emitted on arrival.** This is the part that matters most and the part a
  plain "flush every 16 ms" timer would get wrong: interactive echo — a
  keystroke, a shell's response to it — keeps its pre-#712 latency *exactly*.
  Only a pane that is *already streaming* has its chunks merged.
- **The window is one 60 Hz frame (16 ms).** xterm.js schedules its repaint
  on `requestAnimationFrame`, so two events inside one frame paint once
  anyway: the second buys a script compilation and nothing a human can see.
  16 ms is therefore both the natural period and the entire added-latency
  budget, and it is spent only on a pane that is mid-stream.
- **64 KiB caps one event.** A pane dumping faster than the window can drain
  (a `cat` of a large file) must not grow one unbounded script string. The
  cap is on the emitted batch, not a threshold noticed afterwards — a buffer
  pushed past it drains in cap-sized pieces at that same instant.
- **EOF flushes the residue.** A child that exits mid-window would otherwise
  take its last bytes — a goodbye, a final prompt, an error — with it. The
  reader thread dropping its sender is the EOF signal.

## What is deliberately untouched

- **Bytes and their order.** Only the number of events changes. A terminal
  cannot survive a reordered or truncated escape sequence, so this is pinned
  by test at both layers, not merely intended.
- **The orchestration output ring.** `PtyManager`'s `OutputBuf` is still teed
  on the reader thread the instant bytes arrive, *ahead* of the coalescer.
  Orchestration reads panes through that ring — the attention scan, question
  detection, `get_output` — so none of its timing moves. Coalescing the ring
  as well would have been simpler and would have quietly delayed every
  attention decision by up to a window; the split is the point.
- **The wire shape.** `pty-output` still carries `{id, data: base64}`. `data`
  is just larger sometimes, so nothing on the frontend changes.

## Why the policy is a separate, clock-injected type

`OutputCoalescer` takes `now_ms` as a parameter and never reads a clock.
The pump (`pty_output_pump`) supplies a real monotonic clock and a real
channel, and takes its emit sink as a parameter.

Both choices exist so the two things worth proving can be proven separately:
the *policy* (does a burst inside a window cost one event? does a quiet pane
still emit immediately? do bytes survive?) is pinned deterministically with a
synthetic clock, and the *shipped loop* is pinned by an integration test that
drives the real `pty_output_pump` on its own thread with a counting sink —
no Tauri `AppHandle`, which a headless test cannot have.

## What this does not address

Coalescing bounds the **per-event** cost, which was the unbounded term. The
**per-byte** costs on that path are unchanged by design: base64 inflates the
payload by a third, it is embedded in a JS source string, and the frontend's
`decodeB64` walks it a byte at a time. Those scale with throughput rather
than with the child's write pattern, and they were not what saturated the
thread — but they are where to look next if a single very high-throughput
pane ever becomes the limit.
