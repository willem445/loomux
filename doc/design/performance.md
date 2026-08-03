# Performance architecture

Responsiveness is a structural property here, not a set of fixes. This note is
the rule a new feature inherits by reading one page, and the citable ground a
reviewer bounces a violation from. It is also the **spec** for the two
enforcement tests: `src-tauri/tests/perf_dispatch.rs` (**E1**) and
`test/perfpolicy.test.ts` (**E2**), which land against this spec as #743's
S2/S3. Sections are numbered so a manifest entry can cite one
(`see performance.md §4 X1`).

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

## 2. The proven patterns

Each is shipped, tested, and citable — prefer copying one to inventing a shape.

- **P1 — `spawn_blocking` the whole body.** The command is a thin `async fn`
  whose entire body is handed off, nothing before the first await. Precedent:
  `git.rs` (all 22 commands, #399 + #726), `gh.rs` (all 10 commands, #724),
  `write_pty` (`pty.rs:1552`) and `change_dir` (`:1657`), both #734. `git.rs`
  is the one to copy: it is the largest instance, and the only one whose
  conversion had to give something up — the freeze it removed was also an
  accidental mutual exclusion, so it carries the worked example of restoring
  the ordering (`src/gitqueue.ts`) and of the residual left behind (#754).
- **P2 — Coalesce per frame, leading edge, byte cap.** Bound a
  producer-rate stream backend-side to ≤1 event per pane per 60 Hz frame with
  a hard batch cap, emitting immediately when the producer has been quiet so
  interactive echo keeps its latency. Precedent: `ptyout.rs`
  (`PTY_EMIT_MIN_INTERVAL_MS` = 16, `PTY_EMIT_MAX_BATCH` = 64 KiB, #714).
- **P3 — Leaf locks, request-sized reads.** Take the map lock only to clone a
  handle out, release it, then take the per-item leaf lock; never nest the
  other way. Read the tail you need, not the whole ring. Precedent:
  `PtyManager::writer_handle` (`pty.rs:214-224`, the stated lock order,
  #719/#734),
  `output_tail_bounded` (`pty.rs:453`) and `ATTENTION_SCAN_BYTES` = 4096
  (`orchestration/mod.rs:2341`, #725). Also `QueueMap::mutate`
  (`orchestration/queuestate.rs:163`) and `poll_watches`
  (`orchestration/mod.rs:22578`), which snapshot then release.
- **P4 — Bounded throttle and bounded ladder, with an independent-evidence
  release.** A degraded mode is entered on a signal and left on evidence that
  does not depend on that signal still being right. Precedent:
  `panethrottle.ts` (unfocused panes: leading-edge window plus a
  `MAX_PENDING_BYTES` override; the window itself is
  `DEFAULT_SETTINGS.unfocusedRenderThrottleMs` in `settings.ts`, so the policy
  module owns the policy and settings owns the number — #733) and
  `webglretry.ts` (`WEBGL_RETRY_DELAYS_MS` 2s/10s/60s then stop;
  `WEBGL_HEALTHY_MS` 5 min or a hide/show opens a fresh streak).
- **P5 — rAF dirty-flag on the handler side.** A batch stream sets a flag and
  schedules one `requestAnimationFrame` render instead of rendering per batch.
  Precedent: `FileExplorer.onFilesBatch` (`fileexplorer.ts`, the `ft-files`
  gate); `scheduleRender()` for `ft-search` (`fileedit.ts`).
- **P6 — Backpressure, not queues, for pipes.** A full pipe parks the writer;
  that is the bounded-memory answer. Do not add a backend write queue to
  "smooth" it — an unbounded queue converts a stall into unbounded memory.
  Precedent: `pty.rs:1549` and `doc/design/pty-input-path.md` §2.

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
  **whole** body via `spawn_blocking`/`run_blocking` (P1), or is sync and
  enumerated in E1's `SYNC_COMMANDS` manifest as `(name, class, reason, issue)`
  with class `cheap` (in-memory only), `exception` (§4), or `debt` (an existing
  offender, owning issue named). A new unargued sync command is refused by a
  test, not by a reviewer's memory. Sync commands that hand work to a raw
  `std::thread::spawn` and return are `exception` entries.
  *Enforced: E1 (#743 S2).*
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
  listener with no declared bound is refused. *Enforced: E2 (#743 S3).*
- **INV-4 — Cadenced work declares itself.** Every `setInterval` appears in
  E2's timer manifest with `{cadence, visibility policy: gated |
  component-scoped | argued(reason)}`; a frontend timer that drives IPC or
  rendering is visibility-aware or argued. A backend tick is bounded per tick
  (#656/#695) and never holds a cross-group lock across subprocess IO — the
  merge-queue driver is the argued exception (§4 X4).
  *Enforced: E2 (#743 S3) + review.*
- **INV-5 — Locks on latency-sensitive paths are leaves.** Map lock → release →
  leaf lock; reads are request-sized rather than clone-then-slice; CPU work
  (ANSI strip, parse, JSON format) runs outside the guard; no file IO under a
  lock a poll path takes. See P3. *Enforced: review* (a source scan cannot see
  lock scope; the census in §5 is the standing list).
- **INV-6 — Degraded surfaces recover boundedly.** A coarser mode is never
  staler than its own window, a capped resource is re-acquired on a bounded
  ladder, and any suppression driven by a fallible signal has a release that
  does not depend on that signal. See P4. *Enforced: review* + the pure-module
  unit tests each policy already carries.

## 4. Argued exceptions

These are deliberate and stay. Each is argued **in code** at the cite; an E1/E2
`exception` row's `reason` points here.

| id | subject | cite | the argument | invariant |
|---|---|---|---|---|
| **X1** | `resize_pty` stays sync | `pty.rs:1566-1594` (doc), `:1596` | A sync command inherits arrival ordering from main-thread dispatch, and resizes need it: `shouldResizePty` suppresses only an *identically sized* in-flight call, so two sizes can be outstanding and off-thread could land them in either order, leaving ConPTY at the older geometry with no event to correct it. The bounded-resize claim is marked ASSUMED in-code with its named falsifier. | INV-1 |
| **X2** | `fm_delete_start` uses a dedicated OS thread, not `spawn_blocking` | `filemgr.rs:846-886` (doc), `:887` | `SHFileOperationW` is a Shell/COM API whose STA requirement the main thread was satisfying implicitly (wry `OleInitialize`s it). A generic async pool has no defined apartment state, so the thread enters its own STA for the duration. | INV-1 |
| **X3** | The `thread::spawn`-and-stream family: `ft_search_start`, `ft_files_start`, `fm_hash_start` | `fileedit.rs:1134`, `:1211`, `filehash.rs:221` | Sync commands that start a cancellable streaming walk and return immediately. The work is off the webview thread and the results arrive as bounded batch events (P5 gates the handler side); the shared cancel registry is why they are threads with a flag rather than opaque pool tasks. | INV-1 |
| **X4** | `mq_state_lock` held across git/gh subprocess runs, on the fleet's single gh-poll thread | `orchestration/mod.rs:8849` (doc), `:36636`, `mqdriver.rs:202` | One registry-wide lock is deliberate — the driver services one group per tick, so per-group locks buy no usable concurrency at the cost of a lock-ordering question. Every call is bounded by `MQ_CMD_TIMEOUT` (60 s), and the coupling is self-documented. **Scope of the exception: it costs fleet latency, not GUI latency** — nothing here runs on the webview thread. Decoupling is #748, not a licence to widen this. | INV-4, INV-5 |
| **X5** | The compact-nudge cadence reads every agent's full pty tail | `orchestration/mod.rs:25714-25749` (doc), `:25750` | The elevated cadence is registry-wide and its cost is bounded and stated: ≤256 KiB × 6 wakes/min per agent (sub-1 MiB/s for a large fleet), and the elevated cadence itself cannot run beyond ~20 min by the state machine's own timeouts. Two cheap tightenings are named in-code, neither built speculatively. | INV-5 |
| **X6** | `AUDIT_LOCK` and `creation` are process-global | `orchestration/mod.rs:9903`, `:8965`, `:10077-10083` | Both serialize by design. `AUDIT_LOCK` makes append and rotation one unit and is held only for the open+write, never across orchestration work; the JSON is formatted before the lock is taken. `creation` serializes group id choice against orchestrator registration, which is a correctness requirement, and fires once per group launch. Named so each stays a decision rather than an accident. | INV-5 |

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
that must argue itself.

Owning issues:

| area | issue |
|---|---|
| 16 sync git-shelling commands | #726 |
| remaining sync gesture commands (filemgr, fileedit, uistate, sessions, cliprobe, editor, `spawn_pty`, `dir_info`, voice) | #746 |
| `tasks_lock` architecture — file IO out from under the board family's lock | #747 |
| `mq_state_lock` / single gh-poll-thread decoupling (fleet latency; §4 X4) | #748 |
| `orch_session_roles` unbounded fan-out | #749 |
