# Per-pane render cost: unfocused throttling and WebGL recovery (#720)

Two costs that are paid *per visible pane, per frame*, on the webview's one JS
thread. Neither is an event-rate problem, which is why
[PTY output coalescing](pty-output-coalescing.md) (#712/#714) — which fixed the
event-rate problem and was the dominant contributor — leaves both standing.

Read that note first. This one starts where it stops.

## What #714 left on the table, precisely

#714 bounded the number of `pty-output` events to **at most one per 60 Hz frame
per pane**, which retired every *per-event* cost: the GUI-thread script
compilation, the `atob`, the decode loop, the listener dispatch. What it could
not touch is what each surviving event costs *inside* xterm, and there are
exactly two such costs:

| | scales with | coalesces across frames? |
| --- | --- | --- |
| **Parse** — `WriteBuffer` → the escape-sequence parser | bytes | n/a (it is the bytes) |
| **Render** — `RenderService.refreshRows` → `renderRows` | dirty cells | **no** |

The asymmetry in the right-hand column is the whole lever, and it is worth
being exact about, because it also bounds what this change can possibly buy:

- `RenderService.refreshRows` funnels into `RenderDebouncer.refresh`
  (`@xterm/xterm/src/browser/RenderDebouncer.ts`), which holds one
  `requestAnimationFrame` and merges every row range requested inside that frame
  into a single `renderRows` call. Multiple writes in one frame therefore cost
  **one** render pass. Writes in *different* frames cost one pass **each** —
  nothing merges across the frame boundary.
- `Terminal.write` appends to `WriteBuffer`, which parses in 12 ms slices and
  yields between them (`WRITE_TIMEOUT_MS`,
  `@xterm/xterm/src/common/input/WriteBuffer.ts`). Parse work is a function of
  bytes and of nothing else.

So batching an unfocused pane's writes into one call per 100 ms window
**removes render passes and moves no parse work whatsoever**: the same bytes
still arrive, are still queued, and are still parsed. A pane written to once per
window renders ~6× less often than one written to every frame. That is the
entire mechanism. It follows directly that this change pays in proportion to
rendering's share of the per-frame cost — see "What is measured and what is
argued" below, which does not pretend that share is known.

xterm already handles the easy half. `RenderService` observes the screen element
with an `IntersectionObserver` and sets `_isPaused` when it stops intersecting
(`_handleIntersectionChange`, `browser/services/RenderService.ts`), which covers
a hidden project tab, a maximized-behind sibling, and a docked pane — all
`display: none`. Note that this pauses **rendering only**; those panes still
parse at full rate, and this change does not alter that either. The gap #720
names is the pane xterm cannot see as skippable: on screen, being rendered, in a
grid of six, and not being read.

## The throttle

`src/panethrottle.ts` — pure, DOM-free, `decideFlush`. `src/pane.ts` holds the
chunk list and the timer (`acceptOutput` / `flushOutput` / `wakeOutput` /
`discardOutput`).

**Leading edge, deliberately the same shape as the backend coalescer one layer
down.** A chunk arriving into a *quiet* pane is written on arrival, so a pane
that prints one line every few seconds behaves exactly as it did before this
existed. Only a pane that is *already streaming* has its chunks merged into the
next flush — the same "quiet panes are never penalised" property #714 argued for
interactive echo, applied here to visual latency.

**"Live" means the human is on it.** The grid's active pane (`Pane.setActive`)
is never throttled, and a pane the human types into is woken back to the leading
edge through `markFirstInput` / `markHumanInput` — the file's existing single
answer to "what counts as human input", deliberately reused rather than hooked
onto `onData`, which also fires for xterm's own query auto-replies (#440 B2-R).
Becoming active flushes immediately, so clicking a pane never shows it a window
stale.

**Two bounds.** `MAX_PENDING_BYTES` (1 MiB) writes the backlog out regardless of
the window, so a pane emitting faster than the window drains cannot grow an
unbounded list, and cannot race xterm's own 50 MB `DISCARD_WATERMARK` — which
*throws* rather than degrading. And `windowMs <= 0` is a true bypass, i.e.
exactly the pre-#720 policy.

### The orderings it must not break

Held bytes were produced *before* whatever the pane does next, so anything that
writes to, resets, or re-geometries the terminal flushes first. Three call
sites, each argued at the site:

1. **`doFit`** — the resize is gated behind `term.write("", cb)` specifically so
   bytes produced under the old geometry are parsed before the new one lands
   (#432 item 2). The throttle adds a queue *in front of* xterm's, so held bytes
   must enter xterm's queue before that empty write, or the #432 fix would still
   be in place and still be defeated, one layer up.
2. **`notifyExited`** — the process's last bytes precede the banner announcing
   its exit, and must print above it.
3. **`respawnFresh`** — the one site that **drops** rather than flushes.
   `term.reset()` clears the buffer synchronously while `term.write` parses
   asynchronously, so flushing here would not preserve those bytes: it would
   queue them to be parsed *after* the wipe, painting a dead pty's tail over the
   fresh session. They are bytes `reset()` was always going to erase; the only
   real choice is erased-in-place versus resurrected-in-the-wrong-place.

### The ordering the flush itself must respect

`markFirstInput` flushes **before** `humanOrigin.mark()`, and that order is
load-bearing. `WriteBuffer.write` parses inline — not on a later task — when a
write lands on an empty buffer right after user input (`_didUserInput`, a fast
path that exists to cut echo latency). The origin latch's whole correctness
argument is that data the terminal manufactures for itself "arrives while its
own `term.write()` is being parsed — always a different turn"
([humanorigin.ts](../../src/humanorigin.ts)). Flushing *after* the mark would put
exactly such a write inside the marked turn and hand the backend's keystroke
clock a DA/OSC auto-reply as a human keystroke — the #179/#518 failure,
re-created by a performance change. `test/xterm-syncparse.test.ts` pins both
arms of that upstream behaviour so the constraint is enforced rather than
carried on a comment.

### Two cases that deliberately get no special handling

- **The active pane of a HIDDEN tab is not throttled.** Every tab's grid keeps
  its own active pane, so a background tab has one too. Throttling it would save
  nothing that matters: xterm renders it zero times either way (the
  `IntersectionObserver` pause above), and the throttle's only saving is render
  passes.
- **A `tryWebgl` that throws does not schedule anything.** The ladder advances on
  *losses*, and a machine with no usable WebGL2 path never had a context to
  lose. That pane stays on the DOM renderer until a hide/show, exactly as it did
  before #720 — this change recovers lost contexts, it does not retry
  never-acquired ones.

`receivedOutput` is still latched on **arrival**, not on flush: it answers "did
this process ever print anything" (the DOA signature, #281/#280), which is a
fact about the pty and not about when loomux chose to render it.

Nothing in orchestration is affected. Attention scanning, question detection and
`get_output` read the backend's output ring, which is teed on the reader thread
ahead of even the #714 coalescer — see that note's "Deliberately unchanged".

## The WebGL re-acquire

`src/webglretry.ts` — pure, `planWebglRetry`. Wiring in `Pane.handleWebglLoss` /
`tryWebgl` / `setHidden`.

Before this, `onContextLoss` disposed the addon (correct — the context is gone,
and the DOM renderer is what keeps the pane painting) and stopped there. Nothing
retried. One lost context therefore left **one pane in a grid several times more
expensive to render than its neighbours, permanently and invisibly**, until the
human happened to hide and re-show the project tab — `setHidden` drops and
re-acquires contexts for unrelated reasons (#63), and was the only path back.

**Why the retry has to be bounded.** A WebGL context is a capped resource; the
browser evicts an existing one when a new one is created past the cap. That cap
is why `setHidden` releases contexts for inactive tabs at all. It also means
"pane A lost its context" and "pane B created one" are frequently the *same
event seen from two panes* — so an unbounded retry is not a recovery, it is a
live-lock: A re-acquires, evicting B; B retries, evicting A; forever, each round
paying a context creation and a full texture-atlas rebuild.

**The ladder: 2 s, 10 s, 60 s, then stop.** The scale is set by what has already
happened before the policy is consulted. `WebglRenderer` does not report a loss
immediately: it `preventDefault`s `webglcontextlost` and waits **3000 ms** for a
`webglcontextrestored` the browser may deliver on its own, firing
`onContextLoss` only if none arrives (`@xterm/addon-webgl/src/WebglRenderer.ts`).
The browser's own restoration path has therefore already had its chance and
declined, which is why the first rung is seconds rather than milliseconds — a
sub-second retry races nothing.

**Two releases, both on evidence rather than on a bare clock.** A hide/show
(`setHidden`) clears the streak, because that is a deliberate act that changes
the context situation wholesale — every pane in the outgoing tab just released
one. And a context that stayed alive `WEBGL_HEALTHY_MS` (5 minutes, longer than
the whole ladder) before dying opens a **fresh** streak: that was not a storm,
and without this rule three unlucky losses spread across an eight-hour session
would strand a pane on the DOM renderer for the rest of it — a bound outliving
its own justification.

## What is measured, what is argued, and what the human must confirm

Being explicit, because this is a performance change whose payoff is not a
number this repo can produce on its own.

**Established from source, not inferred** (citations above): that render passes
coalesce within a frame and not across it; that parse cost is a function of bytes
and is therefore untouched by batching; that off-screen panes already skip
rendering but not parsing; that `onContextLoss` fires only after the browser has
spent 3 s declining to restore; that the write path holds and reorders nothing.

**Argued, not measured:** rendering's *share* of an unfocused pane's per-frame
cost. That share decides the size of the win, and it depends on the active
renderer (WebGL vs the DOM fallback — which is exactly what the second half of
this change is about), on the GPU, and on how many panes stream at once. Loomux
cannot measure it in CI: it needs a real WebView2 window with real panes under
real load.

**Which is why `unfocusedRenderThrottleMs` exists.** It is a knob, not a
preference. Setting it to `0` and relaunching restores the pre-#720 behaviour
exactly, so the human can A/B the thing on their own machine instead of
accepting a number loomux asserts. The second half needs no such hedge: a pane
stuck on the DOM renderer is strictly worse than one on WebGL, and recovering it
is an improvement whatever the ratio turns out to be.

**Not done, deliberately.** The Unicode11 grapheme provider was left alone. It
is on the parse path, and the parse path is precisely what this change does not
reduce — so trading its correctness (wide-character and emoji cell widths) for a
saving of unknown size, in a change that could not measure that size, would be a
guess dressed as an optimisation. #720 asked for a measurement first; there is
none, so there is no change.
