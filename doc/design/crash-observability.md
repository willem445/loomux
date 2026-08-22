# Design: crash observability

Status: implemented (issue #53); fail-log robustness hardening in #1219, after
#1218 showed the hook losing exactly the crashes it exists to record.

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

`install_panic_hook(app_version)` runs first thing in `run()`, before any other
setup, so
even a panic during startup is captured. It **wraps** the existing hook (chains
to it, preserving dev-build console output) and, before chaining, writes
`<data>/loomux/logs/crash-<YYYYMMDD-HHMMSS>.log` containing:

- loomux version, UTC timestamp, and **thread name**. The version is the
  argument, not an `env!("CARGO_PKG_VERSION")` inside the hook: `obs` lives in
  `loomux-engine` since #888 slice A3 batch 7, and that macro names the crate a
  file is *compiled in*, so reading it there would report the engine's permanent
  `0.0.0` placeholder instead of the release that crashed. `src-tauri`'s `run()`
  passes its own, where the macro means what it says;
- the panic message and source location (`file:line:col`);
- a backtrace from `std::backtrace::Backtrace::force_capture()` — `force_capture`
  ignores `RUST_BACKTRACE`, so a crash log always carries one.

The hook is installed process-wide, so it fires for panics on **any** thread —
the PTY reader/waiter threads, the MCP request threads, the delivery threads,
and the watchers — not just the main thread. The hook body is wrapped in
`catch_unwind` and every I/O step is best-effort, so the hook can never panic
and mask the original crash.

#### The write is two-phase (#1219)

`catch_unwind` is not the protection it reads as, and that matters for the
order the hook does things in. A panic raised *inside* the hook is a
panic-while-panicking: std's `panic_count::increase` sees its own
`in_panic_hook` flag, returns `MustAbort::PanicInHook`, prints "thread panicked
while processing panic" and **aborts on the spot**. No unwind starts, so
`catch_unwind` never gets control, and the hook is never called a second time.
Everything the hook had not yet written is lost.

That is exactly what #1218 was: the hook captured and symbolicated a backtrace
*first* and wrote the file *afterwards*, so a death inside the capture — dbghelp
plus rustc-demangle plus unbounded allocation, the most fragile code in the
process at the worst possible moment — produced **zero bytes on disk** twice in
two days, on a build whose whole purpose was to leave a record.

So the write is split:

1. **Phase one — the record that has to survive.** Open `crash-<ts>.log`, write
   and flush version / time / thread / panic message / location, and write the
   `panic` breadcrumb. Then, and only then, is anything fragile attempted.
2. **Phase two — everything that can die trying.** `Backtrace::force_capture()`,
   `to_string()`, and a second `write_all` appending `backtrace:` through the
   same handle.

The file is opened `create(true).append(true)` in phase one and phase two writes
through that handle, so the two-phase split changes the *order*, not the format:
the phases concatenate to the same bytes the single write produced, and a crash
log with no `backtrace:` section is a complete record of a crash that died
collecting one.

**Phase one composes without `core::fmt`.** `format!`/`write!` is a deep generic
call tree that runs arbitrary `Display` impls and does its own capacity
arithmetic — `capacity overflow` frames were live at abort time in #1218 — and a
panic down there is the abort above. So the filename, the record and the
breadcrumb line are built out of byte slices and a hand-rolled integer formatter
(`push_dec`, `push_stamp`, `push_spaceless`). The distinction is narrow on
purpose: allocation is fine (an allocation failure aborts however you spell it);
it is the *formatting machinery* that is kept off the path. Nothing behavioural
can catch a `format!` creeping back — it would pass every test and show itself
only as another empty crash log on a user's machine — so
`the_first_phase_composes_without_the_formatting_machinery` scans the marked
source regions for the tokens instead, and states its own blind spot (it cannot
follow a call out of a region; the callees are listed and checked by hand).

**Double-panic fallback.** A thread-local latch (`HookGuard`) is armed for the
whole of a hook run. A re-entrant run does the least possible: one appended
`double-panic: <msg> at <loc>` line to the path phase one already derived — no
backtrace, no directory work, no formatting — and then chains to the default
hook, so the process dies exactly the way std would have made it die. On today's
std that latch is belt-and-braces rather than the load-bearing guard: the common
double panic aborts before the hook is re-entered at all (above). The
load-bearing defence is the write-first ordering; the latch covers a hook
reached outside that path and costs one thread-local read per panic. It is there
because "do nothing on re-entry" is the behaviour that produced a zero-byte
forensic record twice.

**Backtrace symbol quality.** Two profile settings interact, and the one that
actually matters for naming loomux's own frames is **`debug`, not `strip`**:

- `debug = "line-tables-only"` makes rustc emit debug info covering loomux's
  own functions (names + line numbers). **This is the setting that symbolicates
  our frames.** Without it — a `[profile.release]` with no `debug` key defaults
  to `debug = false` — the emitted MSVC `loomux.pdb` carries only public/linker
  symbols, so dbghelp resolves our internal functions as `__ImageBase` **even
  with the PDB sitting right next to the exe**. (Only *std* frames get named in
  that case, because the Rust toolchain ships std's debuginfo separately — which
  is exactly what makes a `debug=false` backtrace *look* symbolicated while every
  loomux frame is really `__ImageBase`.)
- `strip = "debuginfo"` keeps the *exe* slim by not embedding that debug info in
  the binary — it lives in the standalone PDB (Windows) / split debug (Linux) /
  dSYM (macOS). It is orthogonal to naming: with `debug = false`, no `strip`
  value produces named loomux frames; with `debug = "line-tables-only"`, frames
  are named regardless of `strip`.

Verified empirically on this toolchain (`rustc 1.96.0`, `x86_64-pc-windows-msvc`,
`lto = true, codegen-units = 1`, `Backtrace::force_capture()`, an
`#[inline(never)]` marker fn, PDB kept adjacent): `debug = false` → own frame is
`__ImageBase`; `debug = "line-tables-only"` → own frame is named **with line
numbers**. So the profile now sets **both** `debug = "line-tables-only"` and
`strip = "debuginfo"`.

With that, **build-tree and CI** backtraces are genuinely symbolicated (own
frames + line numbers) because `loomux.pdb` sits beside the exe in
`target/release/`. The remaining gap is the shipped **bundle**:
`tauri.conf.json` → `bundle.targets: "all"` (NSIS/MSI) copies only `loomux.exe`,
the conhost resources, and icons into the installer — **not** `loomux.pdb`. So an
**end-user's installed** loomux still produces **address-only** backtraces. Even
there the panic *message*, *location* (`file:line:col`, from panic metadata —
independent of the PDB), and *thread name* are always captured and usually
enough to localize the fault.

**Size cost (measured on the release build).** Enabling `debug =
"line-tables-only"` leaves the shipped **exe essentially unchanged** —
9,822,208 → 9,819,136 bytes (−3 KB) — because `strip = "debuginfo"` keeps the
line-tables out of the binary. The cost lands entirely in the standalone
**PDB**: 2,387,968 → 26,660,864 bytes (**+~23 MB**), which is the line-tables
debuginfo for loomux's own (LTO'd) code — and itself confirms the setting took
effect on loomux's binary, not just std. Since the bundle doesn't ship the PDB,
the **installer payload is unaffected**; the cost is only the build-tree/CI PDB
and any future decision to ship it.

We deliberately do **not** ship the PDB in the installer: it exposes symbols and
would add ~23 MB. Two honest follow-ups (out of scope here): bundle `loomux.pdb`
next to the exe (a `bundle.resources` entry pointing at the build artifact, or a
post-build copy step) so installed builds get named frames too; or set up
server-side symbolication — upload the PDB to a symbol server keyed by module +
address. With `debug = "line-tables-only"` now in place, the "drop the matching
`loomux.pdb` beside the installed `loomux.exe`" workaround *does* symbolicate
loomux frames (it would not have with the old `debug=false` PDB).

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

#### Naming the gap (#1219)

`unclean_prev=true` **with no crash log from that run** is a specific,
diagnosable signature — it is the one #1218 produced — and until #1219 it was
reported to the *user* as a toast and to nobody at all in the durable record.
`check_and_arm` now writes one breadcrumb when it sees it:

```
<stamp> crash-log-gap unclean_prev=true crash_log=none the_previous_run_died_without_the_panic_hook_completing look=%LOCALAPPDATA%\CrashDumps and=EventViewer>WindowsLogs>Application(source:Application_Error)
```

It goes in *before* this run's own `startup` breadcrumb, because it is a
statement about the previous run. Both of its inputs come off the same
`StartupCheck` — `unclean`, and the crash log `newest_crash_log_since` found
under that same sentinel mtime — so the detector cannot disagree with itself in
the window it exists to name. `is_crash_log_gap` is the whole predicate, and
`only_an_unclean_start_with_no_crash_log_is_a_gap` pins all four crossings, so
neither "always" nor "never" can pass.

### Windows Error Reporting is the fallback record

When our own hook writes nothing, something else did. On the shipped platform:

- **Event Viewer → Windows Logs → Application**, source `Application Error`, is
  written for a faulting process with no configuration at all, and it carries
  the **exception code** and the faulting module. It is the record to look for
  first, because it always exists.
- `%LOCALAPPDATA%\CrashDumps` holds a user-mode dump, but **only if local dump
  collection has been enabled** — Microsoft's *Collecting user-mode dumps* says
  of it, verbatim: *"This feature is not enabled by default. Enabling the
  feature requires administrator privileges."* The settings live under
  `HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows\Windows Error
  Reporting\LocalDumps`, whose `DumpFolder` **defaults** to
  `%LOCALAPPDATA%\CrashDumps`. So an empty (or absent) directory is evidence of
  nothing — check the key before concluding anything from it.

**What `0xc0000409` means for a Rust binary.** The code's symbolic name is
`STATUS_STACK_BUFFER_OVERRUN`, and on a Rust binary the name is almost always a
red herring. It is the code a **fast-fail** request surfaces as. Microsoft's
`__fastfail` reference, verbatim: *"User-mode fast fail requests appear as a
second chance non-continuable exception with exception code 0xC0000409, and
with at least one exception parameter. The first exception parameter is the
`code` value."* That first parameter is the `FAST_FAIL_*` constant from
`winnt.h` saying *why*, and `FAST_FAIL_FATAL_APP_EXIT` is **7** — the generic
"this process asked to die".

Rust's `std` aborts through exactly that path (inline asm issuing the
`__fastfail` request; rust-lang/rust#73215 tracks the same for
`panic_abort`). So `0xc0000409` with first parameter `7` is what **every**
Rust-side abort looks like from outside the process: a panic-while-panicking,
an allocation failure, or an explicit `abort()` — indistinguishable from one
another at the OS level, which is the whole reason the in-process record has to
survive.

Reading it in practice: the exception parameters are in the dump or the WER
report's `P` fields, not in the plain "Application Error" line, so treat the
`0xc0000409` code plus **the absence of a `crash-*.log` of ours** as the pair of
facts that says *"the panic hook did not finish"* — not as evidence of memory
corruption. Codes to distinguish it from: `0xc0000005` (`ACCESS_VIOLATION`) is a
real fault, typically the ConPTY/windows-sys FFI layer, and `0xc00000fd`
(`STACK_OVERFLOW`) is the guard page — both of which the hook genuinely cannot
catch (see *Limitation: abort-level failures* below).

Sources: [`__fastfail`](https://learn.microsoft.com/en-us/cpp/intrinsics/fastfail?view=msvc-170),
[Collecting user-mode dumps](https://learn.microsoft.com/en-us/windows/win32/wer/collecting-user-mode-dumps),
[rust-lang/rust#73215](https://github.com/rust-lang/rust/issues/73215).

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
- an explicit `abort()` or an allocation failure;
- a **panic raised inside the hook itself** — std aborts before the hook is
  re-entered (see *The write is two-phase* above), which is why the ordering
  there matters more than any guard inside the hook body.

For these the crash log won't exist, but the **breadcrumb log survives** (it's
flushed per line) and the **unclean-exit notice still fires** (the sentinel is
still present), so there's always *something* to read — the breadcrumb tail
shows what was in flight, and the next launch's `crash-log-gap` breadcrumb names
the contradiction and points at WER (both above).

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
- `records_crash_file_with_context` writes the expected fields, driven through
  the two shipped phases (`record_crash_first_phase` + `append_backtrace`)
  rather than a test-only one-shot wrapper, and pins that they **join** into
  the historical bytes;
- **the write-first ordering** (`the_minimal_record_and_the_breadcrumb_land_
  before_the_backtrace_runs`): the backtrace source is injected, so the test
  reads the log directory from *inside* the capture — the instant #1218's
  process died at — and asserts the record and the `panic` breadcrumb are
  already on disk and carry no `backtrace:` section;
- **the double-panic latch** (`the_hook_reentry_latch_is_thread_local_and_only_
  the_outer_run_disarms_it`): one assertion each for never arming it, disarming
  it from an inner run, making it process-wide, and never clearing it — plus
  `the_emergency_write_appends_one_line_and_never_truncates`, since a fallback
  that clobbered phase one would destroy the record it exists to protect;
- **the startup gap**: `only_an_unclean_start_with_no_crash_log_is_a_gap` pins
  all four crossings of the predicate, and
  `an_unclean_start_with_no_crash_log_names_the_gap_in_a_breadcrumb` pins that
  the startup path writes it exactly once, with the WER pointers;
- **the fmt-free property**, by source scan
  (`the_first_phase_composes_without_the_formatting_machinery`) — nothing
  behavioural can catch a `format!` creeping back into phase one, and the scan
  states its own blind spot;
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
