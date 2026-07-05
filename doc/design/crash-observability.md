# Design: crash observability

Status: implemented (issue #53).

## Problem

loomux hard-crashed during an orchestration session — three CLI panes, two
workers compiling Rust and running frontend builds — and left **zero
forensics**. There was nothing to diagnose from:

- `windows_subsystem = "windows"` in release builds (`main.rs`) means there is
  no console, so a panic message printed to stderr goes nowhere.
- There was no panic hook, no log plugin, and no log file anywhere in
  `src-tauri` — the app had nothing to write a crash to.
- Windows Error Reporting had no "Application Error" entry, which is consistent
  with a Rust panic: by default a panic *unwinds* and the process exits cleanly
  from the OS's point of view, so WER never records a fault.

The audit log (`audit.jsonl`) ended abruptly mid-stream right after two worker
PTYs were spawned back-to-back — a hard stop, not a graceful shutdown.

This change adds the observability so the *next* such crash is diagnosable, and
hardens the most likely single-point-of-failure (mutex-poison cascades).

## What was added (`obs.rs`)

All three facilities are dependency-free — nothing new in `Cargo.toml`, and in
particular nothing that pulls `getrandom`/`bcryptprimitives.dll!ProcessPrng`
(the Windows-10 baseline can't load those; see the Cargo.toml note). Timestamps
are formatted from `SystemTime` via the days-from-civil algorithm rather than a
date crate. Logs live under `<data>/loomux/logs/` — the same `loomux/` root as
orchestration state (`<data>/loomux/orchestration/`).

### 1. Panic hook → crash log

`install_panic_hook()` runs first thing in `run()`, before any other setup, so
even a panic during startup is captured. It **wraps** the existing hook (chains
to it, preserving dev-build console output) and, before chaining, writes
`<data>/loomux/logs/crash-<YYYYMMDD-HHMMSS>.log` containing:

- loomux version, UTC timestamp, and **thread name**;
- the panic message and source location (`file:line:col`);
- a backtrace from `std::backtrace::Backtrace::force_capture()` — `force_capture`
  ignores `RUST_BACKTRACE`, so a crash log always carries one.

The hook is installed process-wide, so it fires for panics on **any** thread —
the PTY reader/waiter threads, the MCP request threads, the delivery threads,
and the watchers — not just the main thread. The hook body is wrapped in
`catch_unwind` and every I/O step is best-effort, so the hook can never panic
and mask the original crash.

**Backtrace symbol quality (platform-dependent).** The release profile was
`strip = true`, which blanks symbols and leaves backtrace frames as bare
addresses everywhere. It is now `strip = "debuginfo"` — but what that buys
depends on the platform:

- **Linux / macOS:** frame names come from the binary's own symbol table, which
  `strip = "debuginfo"` keeps (only debuginfo, and thus line numbers, is
  dropped). Crash-log backtraces name their functions. Real win, small size
  cost.
- **Windows / MSVC (the shipping target):** the exe has no in-binary symbol
  table — names come from a **PDB** resolved at runtime by dbghelp. A release
  build *does* emit `loomux.pdb` beside `loomux.exe` (verified:
  `cargo build --release` produces `target/release/loomux.pdb`; `strip =
  "debuginfo"` strips the exe's embedded debug but leaves the standalone PDB).
  So when the exe is run **from the build tree** — local dev and CI — dbghelp
  finds the adjacent PDB and backtraces **are** named. The gap is the shipped
  **bundle**: `tauri.conf.json` → `bundle.targets: "all"` (NSIS/MSI) copies only
  `loomux.exe`, the conhost resources, and icons into the installer — **not**
  `loomux.pdb`. So on an **end-user's installed** loomux the PDB is absent and
  crash-log backtraces are **addresses only**. The addresses are still useful
  (module + offset), and the panic *message*, *location* (`file:line:col`, from
  panic metadata, independent of the PDB), and *thread name* are always present
  and usually enough to localize the fault.

We deliberately do **not** ship the PDB in the installer: it roughly doubles the
payload and exposes full symbols. Two honest follow-ups (out of scope here):
bundle `loomux.pdb` next to the exe (a `bundle.resources` entry pointing at the
build artifact, or a post-build copy step) so installed builds get named frames
too; or set up server-side symbolication — upload the PDB to a symbol server
keyed by the module + address in the crash log. Until then, a developer
reproducing a shipped crash can drop the matching `loomux.pdb` beside the
installed `loomux.exe` and dbghelp will symbolicate.

### 2. Breadcrumb log

`breadcrumb(event, detail)` appends one timestamped line to
`<data>/loomux/logs/breadcrumbs.log`, rotating to `breadcrumbs.1.log` past 2 MB
(one kept generation) — the same size-triggered, lock-free `O_APPEND` scheme as
the orchestration audit log (`rotate_audit_if_needed`). One generation of 2 MB
is thousands of one-liners: enough to answer "what was in flight at the moment
of death" without unbounded growth.

Instrumented lifecycle events (ids and flags only):

| event | where | detail |
|-------|-------|--------|
| `startup` / `shutdown` | `lib.rs` | version, unclean-prev flag |
| `panic` | `obs.rs` hook | thread + location |
| `pty-open` / `pty-exit` | `pty.rs` | id, size / exit code |
| `pty-resize-fail` | `pty.rs` | id + error (successes intentionally omitted) |
| `agent-spawn` / `agent-bind` / `agent-dead` | `orchestration/mod.rs` | agent/pty ids, role |
| `delivery` | `orchestration/mod.rs` | agent/pty, outcome, timing |
| `mcp-auth-fail` / `mcp-tool-fail` | `orchestration/mcp.rs` | method / tool name |

**Privacy + size constraint.** Breadcrumbs never carry prompt or output
*content*. Prompt text already lives in the audit log (`audit.jsonl`), which is
the record for *what was said*; breadcrumbs are the record for *what happened,
and when*. Keeping content out keeps them small and privacy-safe. Notably there
is **no per-output-byte logging** — the PTY reader thread (the hot path under a
compile flood) is untouched; only open/exit are breadcrumbed.

### 3. Unclean-exit detection + next-launch notice

`check_and_arm()` runs at startup. A `running.lock` sentinel is written at
startup and removed on a clean shutdown (the window `Destroyed` path, *after*
`kill_all`). Finding the sentinel already present at startup means the previous
run died without unwinding to a clean exit. When that happens we locate the
newest `crash-*.log` **whose mtime is at or after the sentinel's own mtime**
(the crashed run's start instant) and stash a notice string in Tauri-managed
`StartupNotice` state. The mtime gate matters: a *hard abort* writes no crash
log, so naming the plain newest log would mis-attribute an older crash from an
earlier run. When nothing qualifies, the notice says so ("no crash log was
written (a hard abort …)") and points at `breadcrumbs.log` instead. The frontend drains it
once via the `take_startup_notice` command and shows an info toast:

> loomux exited unexpectedly last run — crash log at &lt;path&gt;

If the crash aborted without unwinding (no crash log), the notice says so and
points at `breadcrumbs.log` instead.

This is conservative by design: any exit that doesn't run the `Destroyed`
handler (including some abrupt-but-benign terminations) is reported as unclean.
A false "exited unexpectedly" toast is a cheap price for never missing a real
crash.

## Hot-path hardening (mutex-poison cascade)

The crash review's leading hypothesis: a single PTY/registry operation panics
while holding a `Mutex`, poisoning it; then every other thread's
`.lock().unwrap()` on that same mutex panics too, turning one edge-case panic
into a total-app death spiral. `pty.rs` alone had ~15 bare `.lock().unwrap()`
sites; `orchestration/mod.rs` had ~40 more.

**Fix: poison-tolerant locking.** `obs::LockExt::lock_safe()` recovers the guard
from a poisoned mutex (`unwrap_or_else(|e| e.into_inner())`) instead of
propagating the panic. Applied to **every** `Mutex` access in `pty.rs`,
`gitwatch.rs`, and `orchestration/mod.rs` — completeness matters: a single
remaining `.lock().unwrap()` on a mutex would re-arm the cascade for that mutex.

Why this is safe here: the guarded structures are maps/sets of independent
entries (the PTY table, the agent roster, attention sets). A panic mid-mutation
leaves at worst a half-inserted entry — never a memory-unsafe or
logically-catastrophic state — so proceeding on the recovered guard is strictly
better than crashing every thread that touches it. This trades a theoretical
"observe slightly-stale state" for "the app stays up and writes a crash log."

A few concrete `.unwrap()` landmines that could panic *while holding* the agents
lock (a `get_mut(id).unwrap()` / `agent(id).unwrap()` after a lock-release
window where a concurrent reap could remove the entry) were also converted to
graceful `if let Some`/`ok_or` handling.

`cliprobe.rs`'s probe-cache mutex was intentionally left as-is: it's isolated to
CLI probing and can't cascade into the PTY/orchestration paths.

## Limitation: abort-level failures

The panic hook only fires for **unwinding** panics. It will *not* capture:

- a stack overflow (the OS kills the thread; Rust's guard-page handler prints to
  stderr — which is nowhere in a `windows_subsystem = "windows"` build — and
  aborts);
- an FFI/`unsafe` access violation from the ConPTY / windows-sys layer;
- an explicit `abort()` or an allocation failure.

For these the crash log won't exist, but the **breadcrumb log survives** (it's
flushed per line) and the **unclean-exit notice still fires** (the sentinel is
still present), so there's always *something* to read — the breadcrumb tail
shows what was in flight.

Capturing aborts properly needs an OS-level handler. The honest options, none
implemented here to respect the "no heavyweight crates / nothing pulling
getrandom" constraint:

- **Follow-up (cheap, Windows-native):** register a Structured Exception Handler
  / vectored exception handler via the `windows` crate (already a dependency) —
  `AddVectoredExceptionHandler` / `SetUnhandledExceptionFilter` — and on a fatal
  exception write a minimal crash log (exception code + a `RtlCaptureStackBackTrace`
  frame list) before the process dies. This adds no new crate. It's scoped out
  of this PR as a follow-up because it needs careful async-signal-safe handling
  (no allocation in the handler) and live testing against real access violations.
- **Heavier:** a minidump crate (`minidump-writer` / `crashpad`) — rejected: new
  heavyweight dependencies, and the crash-handler crates tend to pull `getrandom`
  transitively, which this Windows-10 baseline can't load.

## Testing

`obs.rs` unit tests (hermetic — core helpers take an explicit dir; the two that
need global state serialize on a test mutex and restore the panic hook):

- **forced panic in a named background thread writes a crash log** capturing the
  thread name and message (the issue's acceptance criterion), plus a `panic`
  breadcrumb;
- `record_crash` writes the expected fields;
- **unclean-exit detection**: first launch clean + arms sentinel; a launch with
  a leftover sentinel reports unclean and yields the notice; a clean exit clears
  it;
- the notice **names only a crash log from the crashed run** (mtime ≥ sentinel):
  a stale pre-sentinel log is *not* named on a hard abort, and a fresh
  post-sentinel log *is* named;
- **`lock_safe` recovers a poisoned mutex**: a thread poisons a `Mutex` by
  panicking under its guard, and `lock_safe()` still serves the recovered data
  without propagating the panic (a direct test of the load-bearing cascade fix);
- **breadcrumb rotation** at the cap (retains one generation) and content
  (event + detail only, single line);
- timestamp formatting is sortable UTC.
