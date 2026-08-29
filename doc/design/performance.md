# Performance architecture

Responsiveness is a structural property here, not a set of fixes. This note is
the rule a new feature inherits by reading one page, and the citable ground a
reviewer bounces a violation from. It is also the **spec** for the two
enforcement tests: `src-tauri/tests/perf_dispatch.rs` (**E1**) and
`test/perfpolicy.test.ts` (**E2**), which land against this spec as #743's
S2/S3. Sections are numbered so a manifest entry can cite one
(`see performance.md §4 X1`).

**How to read a cite here.** The **symbol** is the cite — a function, const,
static or struct field, written as `` `file.rs` `Type::member` ``. Any `~L`
beside it is a navigation hint, deliberately *not* maintained: a hint that has
drifted is not a defect and not a review finding, and re-deriving one is never
a reason to touch this file. Line numbers were the cite until #743 S7, and they
did not survive contact — three PRs re-broke them in a fortnight, and two
(§4 X4, X6) pointed at unrelated code from the day they were written, because
nothing about a wrong number looks wrong. Grep the symbol.

## 1. The model

One thread — the webview/GUI thread — services **all** of: keyboard and mouse
input, xterm parse and paint, the body of every **sync** `#[tauri::command]`,
and every frontend event handler. Tauri polls an `async` command's future on
that thread too, so an `async fn` that does work *before* its first await has
moved nothing (#724).

A Tauri emit is not a data channel: `Emitter::emit` ends in
`Webview::emit_js` → `Webview::eval(emit_js_script(..))` (tauri 2.11.5,
`src/webview/mod.rs`, `src/event/mod.rs`), which inlines the whole payload into
a fresh JS source string; off-thread it wakes the GUI event loop via
`send_user_message` → `proxy.send_event` (tauri-runtime-wry 2.11.4), which then
compiles and runs that one-shot script. `tauri::ipc::Channel` evals too below
its direct-execute threshold. **The cost is per event, not per byte**, and the
only lever is how many events are sent (`src-tauri/src/ptyout.rs`).

The budget follows: **nothing on that thread beyond in-memory work.** A
saturated GUI thread is felt as app-wide sluggishness while total CPU sits at
15-30% — one thread pinned on a many-core box, not a busy machine.

### 1.1 The other scarce resources

For a long time the paragraph above was the whole model, and every invariant in
§3 is written about that one thread. `doc/plans/responsiveness-root-cause.md`
§2.1 is the account of what that cost: the remedy the invariants prescribe is
always *"move it off the webview thread"*, the **destination** was ungoverned,
and three releases in a row moved a stall from one scarce resource to another
while each move looked like a fix.

There are four, and the model names all four:

1. **The webview/GUI thread** — §1 above. Saturating it is felt as
   sluggishness: the window is slow but everything still works eventually.
2. **The shared `spawn_blocking` pool.** Every `run_blocking`/`spawn_counted`
   hand-off in the app lands in ONE pool — every converted `orch_*` poll, every
   gesture command, the `gh` and `git` families. Nothing in the tree sets
   `max_blocking_threads`, so it is tokio's default of **512**.
   Saturating it does not feel like slowness, and beta6 is what that looks
   like: the pty input path was in this pool too, `src/ptywrite.ts` keeps one
   `write_pty` in flight per pane and chains the next on the previous promise
   (#65's ordering guarantee), so a `write_pty` that could not be scheduled
   stopped that pane accepting input **permanently** while the window kept
   painting — every pane at once.
   **The input path is no longer one of this resource's tenants** (#1607, Phase
   2.3): it went to a thread per pane, P8-writer below. That removes the
   *victim* beta6 was noticed through and none of the mechanism — the pool is
   still shared, still 512, and still exhaustible by everything else in the
   list above, which is why this entry stays and why INV-4's single-flight
   sentence matters.
   Every hand-off is counted through `blocking.rs` `spawn_counted` (#1601
   Phase 0.3) and the depth is breadcrumbed at 64/128/256.
3. **The MCP request threads.** `orchestration/mcp.rs` is `tiny_http` with
   `std::thread::spawn` per request; every tool call resolves its token and
   then takes registry locks on that thread. This is the orchestrator's only
   channel, and no invariant here governs it — a hold that parks these threads
   takes the whole fleet down before a human notices anything at all, which is
   why the MCP is the FIRST thing to die in the incident chain and the last
   thing anyone thinks to look at.
4. **Lock acquisition time, as distinct from hold time.** INV-5 bounds what a
   HOLDER does. Nothing here bounds what a WAITER pays: `lock_safe` is
   `Mutex::lock` with poison recovery — no timeout, no try-lock — so a
   caller's cost is set by a lock's worst holder, not by its own body. This is
   the resource #1595 classified five commands against without knowing it
   existed (see `Class::Cheap`'s note in E1). Since #1601 Phase 0.1 every
   registry lock is a `loomux_engine::lockwatch::TrackedMutex`, so a hold past
   five seconds is breadcrumbed with its call site, duration and waiter count
   — and `selfwatch`'s liveness heartbeat separates a stuck GUI thread from a
   starved backend, which is the distinction that cost a release cycle to make
   by hand.

**Naming them is not governing them.** #1601 makes 2 and 4 *observable*; it
bounds nothing and refuses nothing. Resource 2 has since gained its first real
bound — INV-4's single-flight sentence below (#1602/#1604), which stops a poll
path accumulating parked pool threads one per tick — and that is the shape the
rest will take: a rule stated against a resource this section names. What is
still ungoverned is resource 3 entirely, and resource 4 on every path (a
bounded acquisition with a typed `Busy`, an isolated pool for pty writes:
Phases 1 and 2 of the plan). Those are deliberately not written here ahead of
their code, because an invariant over a model that omitted the scarce resource
is exactly what produced a *correct* classification of five commands as `cheap`
that then froze the app.

## 2. The proven patterns

Each is shipped, tested, and citable — prefer copying one to inventing a shape.

- **P1 — `spawn_blocking` the whole body.** The command is a thin `async fn`
  whose entire body is handed off, nothing before the first await. Precedent:
  `git.rs` (all 22 commands, #399 + #726), `gh.rs` (all 11 commands, #724).
  (`pty.rs` `write_pty`/`change_dir` were the #734 instance and are no longer:
  #1607 moved them to **P8-writer** below, because the *destination* — the
  shared pool — is what beta6 exhausted. They are still the reference for the
  half of P1 that is not about the pool: nothing before the first await.)
  `git.rs`
  is the one to copy: it is the largest instance, and the only one whose
  conversion had to give something up — the freeze it removed was also an
  accidental mutual exclusion (INV-7), so it carries the worked example of
  restoring the ordering (`src/gitqueue.ts`) and of the residual left
  behind (#754).
  The delegation helper for a NEW conversion is `blocking.rs` `run_blocking`
  (#746, shared by the nine gesture modules); `git.rs`, `gh.rs` and
  `orchestration/mod.rs` keep their own older private copies, which is history,
  not a pattern to extend.
- **P2 — Coalesce per frame, leading edge, byte cap.** Bound a
  producer-rate stream backend-side to ≤1 event per pane per 60 Hz frame with
  a hard batch cap, emitting immediately when the producer has been quiet so
  interactive echo keeps its latency. Precedent: `ptyout.rs`
  (`PTY_EMIT_MIN_INTERVAL_MS` = 16, `PTY_EMIT_MAX_BATCH` = 64 KiB, #714).
- **P3 — Leaf locks, request-sized reads.** Take the map lock only to clone a
  handle out, release it, then take the per-item leaf lock; never nest the
  other way. Read the tail you need, not the whole ring. Precedent:
  `pty.rs` `PtyManager::writer_handle` (~L224 — the stated lock order,
  #719/#734), `PtyManager::output_tail_bounded` (~L453) and
  `orchestration/mod.rs` `ATTENTION_SCAN_BYTES` = 4096 (~L2367, #725), with
  `STATUSLINE_SCAN_BYTES` = 64 KiB (~L7007) the same shape on the app's
  hottest poll (#743 S7). Also `loomux-engine/src/queuestate.rs` `QueueMap::mutate` (~L163),
  `orchestration/mod.rs` `OrchRegistry::poll_watches` (~L22741) and
  `OrchRegistry::compact_signals_from` (~L25782), which snapshot then release.
  The last of those is pinned by a test rather than by review —
  `tests/perf_leaflocks.rs` holds a pane's output-ring mutex so the read parks
  inside it, then asks whether the registry still answers. That technique is
  the read-side of `PtyWriteGate` (`tests/ptywrite.rs`, #719) and is the way
  to enforce INV-5 on any *new* lock that matters.
- **P4 — Bounded throttle and bounded ladder, with an independent-evidence
  release.** A degraded mode is entered on a signal and left on evidence that
  does not depend on that signal still being right. Precedent:
  `panethrottle.ts` (unfocused panes: leading-edge window plus a
  `MAX_PENDING_BYTES` override; the window itself is
  `DEFAULT_SETTINGS.unfocusedRenderThrottleMs` in `settings.ts`, so the policy
  module owns the policy and settings owns the number — #733) and
  `webglretry.ts` (`WEBGL_RETRY_DELAYS_MS` 2s/10s/60s then stop;
  `WEBGL_HEALTHY_MS` 5 min or a hide/show opens a fresh streak). Two more on
  the handler side (#743): `refreshthrottle.ts`
  (`REPO_SIGNAL_WINDOW_MS` = 500, the window a pane reacts to repo-change
  signals in — leading edge, and the trailing run is what stops the last signal
  of a burst being the dropped one), and `refreshgate.ts`'s `CoalescingRefresh`
  (single-flight plus a trailing merge; its window is the duration of a run, so
  it needs no constant and a slower backend coalesces harder). And the
  suppression case: `pollgate.ts` (#743 S6) stops a poll's interval while the
  window is hidden, and — because `visibilitychange` is a notification and a
  notification can be missed — releases on a re-READ of the current visibility
  state every `HIDDEN_RECHECK_MS` (5 s) rather than on the event that
  suppressed it. The recheck issues no IPC, so the independent release costs
  nothing the suppression was there to save.
  And the coalescing case: `resizeburst.ts` `planFit` (#1149) holds a pane's
  fit back while its geometry is still moving — trailing edge, so an animated
  layout change collapses to one xterm reflow and one `ResizePseudoConsole`
  instead of one per frame — and releases on `FIT_MAX_WAIT_MS` (400) measured
  from the start of the burst, because a gesture with no settled geometry (a
  window-edge drag) would otherwise withhold the fit for as long as the human
  holds the mouse. The bound is the clock rather than the burst signal, which
  is the same independent-release rule `pollgate.ts` follows. Its predecessor
  is the counter-example worth keeping: a fixed 16 ms debounce, narrower than
  the once-per-frame `ResizeObserver` delivery it debounced, coalesced
  nothing at all.
- **P5 — rAF dirty-flag on the handler side.** A batch stream sets a flag and
  schedules one `requestAnimationFrame` render instead of rendering per batch.
  Precedent: `FileExplorer.onFilesBatch` (`fileexplorer.ts`, the `ft-files`
  gate); `scheduleRender()` for `ft-search` (`fileedit.ts`); `framegate.ts`
  (`FrameGate`), the same shape as an injectable-scheduler module so the
  coalescing itself is unit-testable — it gates the `fm-hash` column (#743).
- **P6 — Backpressure, not queues, for pipes.** A full pipe parks the writer;
  that is the bounded-memory answer. Do not add a backend write queue to
  "smooth" it — an unbounded queue converts a stall into unbounded memory.
  Precedent: `pty.rs` `PtyManager::enqueue_frontend_write`'s doc (the argument,
  moved there by #1607 with the seam) and `doc/design/pty-input-path.md` §2.
  Note what P8-writer below does NOT relax: that queue is legal precisely
  because its job carries a completion reply, so the command still resolves on
  the bytes and the pane still stops accepting chunks when its child stops
  draining.
- **P7 — A dispatch ticket, when the conversion took an ORDER away.** P1
  removes an accidental mutual exclusion: one thread ran every command body, so
  arrival order *was* application order and nothing had to say so. Where
  something depended on that, restore the order rather than the exclusion — a
  mutex gives back "not at the same time", never "the later one wins". The
  shape: claim a monotonic ticket in the command **before the first await**
  (that poll runs on the webview thread, so it is stamped in arrival order),
  carry it into the blocking body, and have the body decline if a newer ticket
  has since been claimed for the same subject. Precedent, both #746:
  `uistate.rs` `write_atomic_seq` (per-path high-water mark — an older tab
  layout landing second would otherwise stick, because `persistTabs` never
  re-offers bytes it already issued) and `gitwatch.rs` `GitWatcher::claim`
  (per-pane, where the sharper case is not a stale repoint but `git_unwatch`:
  a pane closed mid-flight would have its watch REINSTALLED, leaking a poll
  target for the life of the process — so the close claims a ticket too, which
  is what makes it a tombstone rather than a gap). Reach for this only when an
  order was actually load-bearing; most conversions find their guard already
  written (a lock, an atomic, an idempotent write) and owe only the sentence
  naming it.
- **P8-writer — a dedicated owner thread with a completion reply.** When a
  command's body must not compete for the *shared* blocking pool, give the
  subject its own thread and keep the completion semantics: the command posts a
  job carrying a reply channel and awaits that reply, so it still resolves when
  the work is actually done. Precedent: `pty.rs` `spawn_pane_writer` — the
  thread it starts beside each pane's reader thread — with `PtyHandle`'s
  `writer_jobs` sender, the `WriterJob` it carries, and
  `PtyManager::enqueue_frontend_write` / `enqueue_cd` (#1607, epic #1600 Phase
  2.3); `doc/design/pty-input-path.md` § "719 revisited on isolation" carries
  the argument. There is no `PaneWriter` *type* — the pattern has a name and the
  pane's writer does not, because it is a thread plus a channel rather than a
  struct; grep the four symbols above.
  **When to reach for it, which is rarely.** P1 is still the default: the pool
  exists so a hundred one-off command bodies do not each own a thread. This is
  for a path with a named OWNER, a long life and a latency contract the pool
  cannot honour — here, one pane's stdin channel, which ConPTY's own guidance
  says to service on its own thread anyway. The disqualifying question is
  "how many of these can exist?": bounded by something the UI bounds (panes,
  windows) is fine, unbounded by a request rate is not — that is a thread leak
  wearing a pattern's clothes.
  **What it costs, stated so the trade is visible.** A thread per subject, and a
  second place work can queue. Both are why the completion reply is not
  optional: a queue that returns at hand-off would make the command's promise a
  claim about bytes that may never be written, which is exactly the shape P6
  refuses.

## 3. The invariants

Each names where it is enforced: **E1** (`perf_dispatch.rs`), **E2**
(`perfpolicy.test.ts`), or **review** (argued in the PR, cited from here).

Both tests are source scans plus a declared manifest, and both owe two things.
**A vacuity guard**, so a drifted regex fails instead of passing over nothing:
E1's discovered command-name set must *equal* `command_manifest::APP_COMMANDS`,
and E2's scan must find its known specimens. And **a stated bound**: a source
scan cannot follow call chains, so a sync command whose *helper* spawns is not
mechanically caught. The manifest review discipline carries that residue; the
scan pins the shape.

- **INV-1 — Command dispatch.** Every `#[tauri::command]` either delegates its
  **whole** body via `spawn_blocking`/`run_blocking` (P1) or to a dedicated
  owner thread with a completion reply (P8-writer), or is sync and
  enumerated in E1's `SYNC_COMMANDS` manifest as `(name, class, reason, issue)`
  with class `cheap` (in-memory only), `exception` (§4), or `debt` (an existing
  offender, owning issue named). A new unargued sync command is refused by a
  test, not by a reviewer's memory. Sync commands that hand work to a raw
  `std::thread::spawn` and return are `exception` entries.

  **`cheap` bounds the critical section, not the acquisition** (#1595). A
  command may be in-memory only and still park the webview thread for an
  unbounded time, because `lock_safe` is `Mutex::lock` with poison recovery —
  no timeout, no try-lock — and the registry mutexes are shared with background
  threads. So `cheap` requires a second, non-mechanical answer: **no lock a
  background thread can hold, OR not on a poll path.** Both fail it. #1592 and
  #1595 were the same defect with opposite work profiles — one expensive, one
  genuinely trivial — which is the evidence that the work was never the
  property that mattered. E1's scan cannot see either half (one is a call-chain
  fact, the other lives in the frontend); review owns it.
  *Enforced: E1 (#743 S2) + review.*
- **INV-2 — No process spawn and no network round trip on the webview thread,
  ever.** No class permits it: `cheap` bodies additionally must carry no
  `Command::new` / `ShellExecuteW` / `.output(` / `fs::` marker, and only a
  `debt` entry may name one, each pointing at its conversion issue. Webview-
  thread file IO is an `exception` only with a stated bound.
  *Enforced: E1 (#743 S2) + review.*
- **INV-3 — Producer-rate event streams are bounded before the webview pays.**
  A stream whose rate is set by an external producer (child output, agent
  activity, a walk) is bounded backend-side by P2 or handler-side by P5. Every
  `listen()` appears in E2's stream manifest with `{rate class, bound:
  backend-coalesced | rAF-gated | throttled | argued-none(reason)}`; a new
  listener with no declared bound is refused.
  **A stream that drives a VIEW's refresh answers INV-4's visibility question
  too** (#1318): what does it do when nobody is looking at that view? The waker
  being a `listen()` rather than a `setInterval` changes the mechanism, not the
  question — and a rule stated for polls alone, on one view's registration
  rather than on the interface, is what left the task board and the NEEDS-YOU
  panel refetching and rebuilding off screen on every agent write for a whole
  session. `src/wakegate.ts` is that answer for an event-driven view, as
  `src/pollgate.ts` is for a timer; note the two answer different questions —
  `pollgate` reads WINDOW visibility, `wakegate` reads whether the view's own
  PANEL is open, and neither sees a background tab or a minimized pane holding
  an open panel (#1465).
  *Enforced: E2 (#743 S3).*
- **INV-4 — Cadenced work declares itself.** Every `setInterval` appears in
  E2's timer manifest with `{cadence, visibility policy: gated |
  component-scoped | argued(reason)}`; a frontend timer that drives IPC or
  rendering is visibility-aware or argued. A backend tick is bounded per tick
  (#656/#695) and never holds a cross-group lock across subprocess IO — the
  merge-queue driver is the argued exception (§4 X4).
  A poll body's calls are additionally never allowed to overlap themselves
  (#1602): a tick firing while its own previous call is still outstanding is
  coalesced rather than issuing a second concurrent call, so a stuck backend
  cannot accumulate parked `spawn_blocking` threads one per tick (the beta6
  mechanism, EPIC #1600 §1.2). `src/singleflight.ts`'s `SingleFlight` (skip,
  no rerun) covers a poll body with no other caller; `src/refreshgate.ts`'s
  `RefreshGate` (skip + exactly one trailing rerun) covers one also reachable
  from a human gesture, so a dropped tick never drops a click. E2's timer
  manifest (`test/perfpolicy.test.ts`) pins which rows use which.
  *Enforced: E2 (#743 S3) + review.*
- **INV-5 — Locks on latency-sensitive paths are leaves.** Map lock → release →
  leaf lock; reads are request-sized rather than clone-then-slice; CPU work
  (ANSI strip, parse, JSON format) runs outside the guard; no file IO under a
  lock a poll path takes. See P3.
  **Latency-sensitive means cadenced, or on the webview thread, or both** —
  a timer, an event stream, a poll, any sync command body. It is not "every
  read in the codebase": an on-demand path that runs when an agent asks and
  that nothing waits on is outside this invariant, and a whole-ring read there
  can be the *right* answer (`orchestration/mod.rs`
  `OrchRegistry::agent_output_tail`, ~L36577, is the
  worked case — #520's grid replay reports blank cells if the escape stream
  starts mid-way, so bounding it would change what the caller sees, not just
  what it costs). Scope is the thing to establish first; a bound that costs
  fidelity for a cost nobody pays is not a win.
  *Enforced: review, and by test where the read can be held still* (a source
  scan cannot see lock scope — see P3's last paragraph for the technique;
  the census in §5 is the standing list).
- **INV-6 — Degraded surfaces recover boundedly.** A coarser mode is never
  staler than its own window, a capped resource is re-acquired on a bounded
  ladder, and any suppression driven by a fallible signal has a release that
  does not depend on that signal. See P4. *Enforced: review* + the pure-module
  unit tests each policy already carries.
- **INV-7 — A conversion that deletes an exclusion argues it, per command.**
  P1 removes an accidental mutual exclusion along with the freeze; **P7**
  carries the shape and the ticket remedy. What this invariant adds is *who
  owes the argument and where*: it is owed **in code, above each converted
  command** whose body touches state another command also touches (an index, a
  file, a remote ref), and it is one of three — something else already
  serializes it (a lock the body itself takes; #752's per-command
  `Reentrancy` docs are the worked set), the ordering is restored at the one
  choke point every caller already passes (`src/gitqueue.ts`, bounded per
  INV-6), or the body is stateless. Per command, never per module: the
  stateless argument that carries `gh.rs` (#724) does not carry `git.rs`
  (#726/#744, residual #754). *Enforced: review.*

- **INV-8 — What a long session RETAINS is bounded, and released by a rule that
  needs no memory.** INV-3 and INV-4 bound how *often* the webview pays; neither
  says anything about how long what a handler captured stays reachable, and
  #1301 is the gap between them — a many-hour session hit a webview OOM with
  every stream correctly rate-bounded. Two halves.
  **(a) Per-entity state declares where it is released.** A module-level
  collection keyed by a pane, pty id, agent id, group id or task id names its
  release site in its own doc comment; one keyed by an *object* is a `WeakMap`
  unless it is iterated (`src/main.ts` `sshReconnectLatches`,
  `resumeFallbacks`). A buffer whose producer is external is capped in the units
  it grows in — bytes and entries, each with the number stated, because a held
  chunk costs a wrapper whose size is independent of its payload and a
  bytes-only cap is therefore one a real input shape walks around
  (`src/ptyroute.ts` `MAX_PREATTACH_BYTES`, `MAX_PREATTACH_ENTRIES`,
  `MAX_PREATTACH_IDS`; `src/panethrottle.ts` `MAX_PENDING_BYTES`).
  **A cap that can discard a whole entity's buffer records what is lost HERE,
  beside the guarantee, not only at the constant that takes it.**
  `MAX_PREATTACH_IDS` evicts the oldest waiting id outright, so the
  lossless-startup guarantee that buffer exists for holds only while fewer than
  `MAX_PREATTACH_IDS` ids are concurrently unattached; past that, a pane that
  later attaches can lose its process's first bytes. Reaching it means 64 spawns
  in a row went unattached, which is a frontend already failing the way #1301
  failed — the point of recording it is that "oldest first" is a choice about
  which entity loses, not a reason none does.
  **(b) Teardown does not depend on the tearing-down party remembering a key.**
  This is the half that actually failed. `Pane.dispose` released the per-pty
  routers by `this.ptyId`, which is correct exactly until the pane respawns in
  place and forgets the id it used to hold — after which a module-level map held
  that pane, its terminal and its whole scrollback permanently, with nothing
  left in the app able to name it. The fix is not a remembered extra detach: it
  is keying on the OWNER, so binding to a new id releases the old one and there
  is nowhere for a stale entry to be left (`src/ptyroute.ts`
  `PtyRouter::attach`, `PtyRouter::releaseOwner`).
  *Enforced: the type system for (b) on this router — `attachOutput` takes an
  owner, so no call site can omit it — plus `test/ptyroute.test.ts` and
  `test/transport.test.ts`'s respawn case for the policy; review for (a).*
  A source scan is deliberately NOT claimed: deciding which of `src/`'s maps are
  entity-keyed is a reading job, not a regex, the same residue E2's own doc
  states for self-rescheduling timers. The standing debt this invariant names
  but does not close is in §5.

- **INV-9 — What one TICK carries is bounded by LIVE work, not by session
  length.** INV-3 and INV-4 bound how *often* the webview pays; INV-8 bounds
  what a handler retains once it has it. Neither says how big one tick may be,
  and #1317 is the gap between them: three polled reads whose per-tick size was
  a direct function of how long the human had been running. None was a leak —
  each is replaced wholesale every tick, so nothing accumulates — which is
  precisely why INV-8 does not reach them: it is allocation churn proportional
  to session length, on a fixed cadence.
  **A polled read is sized by the population its READER indexes**, which is
  something to go and measure rather than a cap to guess: in all three cases
  the consuming view provably never looked at the rows that made the payload
  grow. **Where such a read folds a population away it stays legible** — it
  keeps that population's totals, names its size, and RENAMES the key it
  narrowed, so a reader written against the old shape fails loudly instead of
  quietly rendering a subset (`mcp::summarize_group_usage`'s `rest` count,
  #866, is the precedent). See doc/design/polled-payload-shapes.md for the four
  worked cases, including the one this deliberately does not close (#1472,
  §5) — a bound that would cost the reader data it structurally needs is a
  retention question, not a payload one.
  *Enforced: review* + each read's own wire-shape tests
  (`tests/orchestration.rs`: the live-usage view, the board's note split, the
  needs-you join; `test/auditstore.test.ts` for the shared read).

## 4. Argued exceptions

These are deliberate and stay. Each is argued **in code** at the cite; an E1/E2
`exception` row's `reason` points here.

| id | subject | cite | the argument | invariant |
|---|---|---|---|---|
| **X1** | `resize_pty` stays sync | `pty.rs` `resize_pty` + its doc (~L1624) | A sync command inherits arrival ordering from main-thread dispatch, and resizes need it: `shouldResizePty` suppresses only an *identically sized* in-flight call, so two sizes can be outstanding and off-thread could land them in either order, leaving ConPTY at the older geometry with no event to correct it. The bounded-resize claim is marked ASSUMED in-code with its named falsifier. | INV-1 |
| **X2** | `fm_delete_start` uses a dedicated OS thread, not `spawn_blocking` | `filemgr.rs` `fm_delete_start` + its doc (~L887) | `SHFileOperationW` is a Shell/COM API whose STA requirement the main thread was satisfying implicitly (wry `OleInitialize`s it). A generic async pool has no defined apartment state, so the thread enters its own STA for the duration. | INV-1 |
| **X3** | The `thread::spawn`-and-stream family: `ft_search_start`, `ft_files_start`, `fm_hash_start` | `fileedit.rs` `ft_search_start` (~L1134), `ft_files_start` (~L1211); `filehash.rs` `fm_hash_start` (~L221) | Sync commands that start a cancellable streaming walk and return immediately. The work is off the webview thread and the results arrive as bounded batch events (P5 gates the handler side); the shared cancel registry is why they are threads with a flag rather than opaque pool tasks. | INV-1 |
| **X4** | `mq_state_lock` held across git/gh subprocess runs, on the fleet's single gh-poll thread | `orchestration/mod.rs` `OrchRegistry::mq_state_lock` + its doc (~L8914); the holding sites `queue_merge_with` (~L35967) and `mq_drive_group_with` (~L36346); `crates/loomux-engine/src/mqdriver.rs` `MQ_CMD_TIMEOUT` (#888 batch 12a moved it out of `orchestration/`) | One registry-wide lock is deliberate — the driver services one group per tick, so per-group locks buy no usable concurrency at the cost of a lock-ordering question. Every call is bounded by `MQ_CMD_TIMEOUT` (60 s), and the coupling is self-documented. **Scope of the exception: it costs fleet latency, not GUI latency** — nothing here runs on the webview thread. Decoupling is #748, not a licence to widen this. | INV-4, INV-5 |
| **X5** | The compact-nudge cadence reads every agent's full pty tail (outside the `agents` lock since #743 S7 — the whole-ring *read* is the exception, not the lock scope) | `orchestration/mod.rs` `OrchRegistry::any_compact_pending` + its doc (~L25955) | The elevated cadence is registry-wide and its cost is bounded and stated: ≤256 KiB × 6 wakes/min per agent (sub-1 MiB/s for a large fleet), and the elevated cadence itself cannot run beyond ~20 min by the state machine's own timeouts. Two cheap tightenings are named in-code, neither built speculatively. | INV-5 |
| **X6** | `AUDIT_LOCK` and `creation` are process-global | `orchestration/mod.rs` `AUDIT_LOCK` (~L10040), `append_audit` (~L10212), `rotate_audit_if_needed` (~L10070); `OrchRegistry::creation` (~L9102) | Both serialize by design. `AUDIT_LOCK` makes append and rotation one unit and is held only for the open+write, never across orchestration work; the JSON is formatted before the lock is taken. `creation` serializes group id choice against orchestrator registration, which is a correctness requirement, and fires once per group launch. Named so each stays a decision rather than an accident. | INV-5 |
| **X7** | `liveness_stamp` stays sync | `liveness.rs` `liveness_stamp` + its doc; `src/liveness.ts` `startLiveness`; `crates/loomux-engine/src/selfwatch.rs` `liveness` | The blocking pool is one of the two things this command MEASURES (§1.1 resource 2). Delegated, it would stop running at exactly the moment the pool is exhausted, and the heartbeat would then report the webview thread stuck on the one occasion it is the only healthy half left — the opposite of the truth, on the reading a diagnosis would rest on. The body is six relaxed atomic stores and a monotonic clock read: no allocation, no IO, and **no `Mutex` at all**, so there is no acquisition to park on either (the half of `cheap` #1595 added). Filed here rather than as `cheap` because the synchrony is a *requirement*, and a future sweep that mechanically converts the `cheap` rows must not take it. Its 1 Hz timer is also the one in E2's manifest that must NOT be visibility-gated — a hidden window is a state the app can be frozen in — which is argued at that row and at `src/liveness.ts`. | INV-1, INV-4 |


An exception is not a precedent. A new one needs its own argument, in code at
the site, and a row here.

## 5. The debt register

The complete main-thread hazard census — every command with its class and
cite (reconciled against `command_manifest::APP_COMMANDS`, so none escaped the
walk), every frontend listener, every timer, and every lock on a
latency-sensitive path — lives in the planning comments on **#743** (parts 1,
2, 2b, 2c) and is **the** source of truth. It is not duplicated here, and
should not be duplicated anywhere: a second copy goes stale silently.

E1's `debt` tier is seeded verbatim from that census, so the register is
executable — deleting a row is the roadmap, adding one is a review-visible diff
that must argue itself. **That tier is empty as of #1592**, which converted the
last row (`orch_session_roles`). An empty tier is not an empty register: the
table below still owns work, and E1's forwards half still refuses the next
unargued sync command.

Owning issues:

| area | issue |
|---|---|
| 16 sync git-shelling commands | #726 |
| xterm scrollback: 13-25 MB per pane, never trimmed for an exited or docked one (INV-8a) | #1315 |
| six module-level collections with no prune (INV-8a) | #1316 |
| the human board's ROW COUNT on a poll — a retention decision, not a payload one (INV-9; #1317 lifted the payloads and argued this half out of scope) | #1472 |
| embed views left OPEN in a background tab or a minimized pane still refetch and re-render (#1318 closed the closed-panel half only) | #1465 |
| `tasks_lock` architecture — file IO out from under the board family's lock | #747 |
| `mq_state_lock` / single gh-poll-thread decoupling (fleet latency; §4 X4) | #748 |
| `orch_session_roles` fan-out still scales with groups EVER created — #1592 took it off the webview thread and stopped it slurping both audit generations per group, so what remains is the index or live-groups filter #749's scope actually named | #749 |
