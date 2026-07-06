# Design: Windows Job Object kill-on-close teardown

Status: implemented (issue #78, phase W1).

## Problem

On Windows, terminating a process does **not** terminate its descendants. A
loomux agent pane is a chain — `OpenConsole → pwsh (wrapper) → claude.exe → …`
(bash/node children while the agent works). When a pane is killed, loomux calls
`TerminateProcess` on the **direct** child only (portable-pty's `ChildKiller`);
descendants are left to the best-effort cascade of ConPTY teardown (closing the
master hangs up the pseudoconsole). That cascade is not reliable.

The #78 investigation found the failure live: **orphaned `pwsh` wrappers still
parenting live `copilot.exe` agents whose parent PID was already dead**, and —
since then — leaked agents plus an orphaned `vite` squatting port 1420 after a
pane kill. Dead panes were leaving live processes: burning model connections,
holding ports, and inflating the process count the issue is about.

Unix has no equivalent bug (see *Unix* below), so this is a Windows-only fix.

## Approach

Enroll each pane's spawned child in a **Job Object** created with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. A job is a kernel-managed set of
processes; with that limit flag, **when the last open handle to the job closes,
the kernel terminates every process still in the job.** Child processes a job
member spawns join the job automatically. So one handle, held for the pane's
lifetime and dropped on teardown, gives deterministic whole-subtree reaping.

This is strictly additive and **fail-soft**: if anything in job setup fails, the
pane spawns exactly as before (pre-#78 behavior) and a breadcrumb records it. A
job is never allowed to fail a spawn.

## Implementation (`src-tauri/src/pty.rs`)

- **`JobHandle`** — a Windows-only newtype owning the job `HANDLE`; its `Drop`
  calls `CloseHandle`, which is what fires kill-on-close. It is `Send + Sync`
  (a plain owned kernel handle, no aliasing) so it can live in the
  `PtyManager` map behind a `Mutex`.
- **`assign_kill_on_close_job(pid) -> Option<JobHandle>`** —
  `CreateJobObjectW` (anonymous, null security) →
  `SetInformationJobObject(JobObjectExtendedLimitInformation)` with the
  `KILL_ON_JOB_CLOSE` limit flag → `OpenProcess(PROCESS_SET_QUOTA |
  PROCESS_TERMINATE)` → `AssignProcessToJobObject`. Any failure closes what was
  opened and returns `None`. Bindings come from the in-tree `windows` crate
  (v0.57, pure `windows-sys` underneath — **no getrandom**, which the Windows 10
  baseline can't load; see `Cargo.toml`). Features added:
  `Win32_System_JobObjects`, `Win32_System_Threading`, `Win32_Security`
  (the last only for `CreateJobObjectW`'s `SECURITY_ATTRIBUTES` arg — we pass
  null).
- **`spawn_pty`** — right after `spawn_command`, calls
  `assign_kill_on_close_job(child.process_id())`; on `None` it breadcrumbs
  `pty-job-fail` and proceeds. The returned handle is stored in a Windows-only
  `PtyHandle._job` field.
- **Teardown is drop-driven.** `PtyManager::{kill, kill_all}` and the waiter
  thread's natural-exit path all `remove` the `PtyHandle` from the map; that
  drops `_job`, closes the job, and reaps the subtree. So pane kill, `end_group`
  (kills each member pty), `kill_all` (app shutdown), and even an agent that
  exits on its own leaving a lingering grandchild (the squatting `vite`) are all
  covered by the same mechanism.

## What is deliberately *not* covered

- **open-in-editor stays unaffected.** The editor is spawned directly via
  `std::process` (not the pty) with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`
  so it outlives the pane and loomux — see [open-in-editor.md](open-in-editor.md).
  It never goes through `spawn_pty`, so it is never in any per-pane job; this
  change does not touch it.
- **Assignment race.** Only children spawned *after* a process joins a job
  inherit the job. We assign synchronously right after spawn, before the child
  has had time to fork, so its subtree is captured — but a grandchild born in
  the microscopic window between spawn and assignment would escape. This is
  best-effort by construction and no worse than today. Phase **W2** (direct-CLI
  spawn, now landed — see the "Pane process model" section of
  [orchestration.md](orchestration.md)) removed the intermediate wrapper shell,
  so for agent panes the enrolled child is the agent itself and *its* children
  are captured from birth.
- **loomux inside a job (Windows Terminal, CI).** Windows 8+ allows a process to
  belong to multiple (nested) jobs, so creating a per-pane job for a child of a
  loomux that is itself in a job just nests — no breakaway needed. The Win10
  baseline satisfies Win8+.

## Unix

No change and none needed. portable-pty's Unix child calls `setsid()` and claims
the pty as its controlling terminal, so it is a **session leader**. Dropping the
master (which happens when `PtyHandle` drops) hangs up the terminal, and the
kernel delivers `SIGHUP` to the whole foreground process group — the subtree
goes down together. The Job Object code is entirely behind
`#[cfg(target_os = "windows")]` and compiles to nothing elsewhere.

## Tests (`src-tauri/tests/job_object.rs`, Windows-gated)

- **`kill_on_close_job_reaps_the_whole_descendant_tree`** — opens a real ConPTY
  the way `spawn_pty` does, runs a shell that spawns a long-lived **grandchild**
  (recording its PID), enrolls the shell via the production
  `assign_kill_on_close_job`, then drops **only** the job handle while keeping
  the ConPTY master + child alive — so a pass can only be explained by
  kill-on-close, not ConPTY teardown — and asserts via `Get-Process` that the
  grandchild is gone.
- **`assign_job_is_fail_soft_on_a_bad_pid`** — enrolling a nonexistent PID
  returns `None` (the fail-soft contract), never panics or leaks.
