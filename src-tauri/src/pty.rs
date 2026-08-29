//! PTY management on top of portable-pty (WezTerm's PTY layer).
//! Uses ConPTY on Windows and forkpty on Unix, so escape sequences,
//! colors, and wide characters behave exactly as a native terminal.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::obs::LockExt;

/// Cap on the per-pty output ring used by orchestration's `get_output` —
/// enough for a few screens of TUI history without unbounded growth.
///
/// `pub(crate)` for orchestration's Tier 1 scan (#685): a read that widens
/// itself needs a ceiling, and the ring is the only honest one — no request
/// past it can return a byte the ring does not hold.
pub(crate) const OUTPUT_RING_CAP: usize = 256 * 1024;

/// Windows Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (issue #78).
///
/// On Windows, terminating a process does NOT terminate its descendants, and
/// ConPTY teardown only *best-effort* cascades — the investigation found dead
/// panes leaving live agents + a squatting vite (issue #78 §5). Enrolling the
/// spawned pane child in a kill-on-close job flips that to a guarantee: when
/// the last handle to the job closes, the kernel terminates every process
/// still in it — the pane's whole descendant tree.
///
/// `PtyHandle` owns exactly one of these, so dropping the handle (pane kill,
/// `end_group`, `kill_all`, or a natural exit that removes it from the map)
/// closes the job and reaps the subtree. Intentionally-surviving spawns —
/// notably open-in-editor, which uses its own DETACHED `std::process` spawn and
/// never goes through the pty — hold no job handle and are unaffected.
#[cfg(target_os = "windows")]
pub struct JobHandle(windows::Win32::Foundation::HANDLE);

// The wrapped value is a plain owned kernel handle; the struct lives in the
// PtyManager map behind a Mutex, so it must cross threads. Nothing aliases the
// handle, so moving/sharing it is sound.
#[cfg(target_os = "windows")]
unsafe impl Send for JobHandle {}
#[cfg(target_os = "windows")]
unsafe impl Sync for JobHandle {}

#[cfg(target_os = "windows")]
impl Drop for JobHandle {
    fn drop(&mut self) {
        // Closing the last open handle to the job is what fires
        // KILL_ON_JOB_CLOSE and tears the subtree down.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Create a kill-on-close Job Object and enroll process `pid` in it, returning
/// the owning handle. Fail-soft: any failure returns `None` (the caller
/// breadcrumbs and keeps today's behavior — never fail the spawn).
///
/// Note the assignment race: only children a process spawns *after* it joins a
/// job inherit the job. We enroll the freshly-spawned pane child synchronously,
/// before it has had time to fork, so its subtree is captured; a grandchild
/// born in the microscopic window before assignment would escape. Direct-CLI
/// spawn (issue #78 W2) removes the intermediate wrapper shell, making the
/// agent itself the enrolled child. If loomux itself runs inside a job
/// (Windows Terminal, CI), nested jobs handle this — allowed on Win8+, which
/// the Win10 baseline satisfies.
#[cfg(target_os = "windows")]
pub fn assign_kill_on_close_job(pid: u32) -> Option<JobHandle> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

    unsafe {
        // Anonymous job, default (null) security attributes.
        let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            core::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .is_err()
        {
            let _ = CloseHandle(job);
            return None;
        }

        // Just enough rights to enroll the child. It's held alive by the
        // caller's child/killer, so its PID can't recycle before this runs.
        let Ok(proc) = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) else {
            let _ = CloseHandle(job);
            return None;
        };
        let assigned = AssignProcessToJobObject(job, proc);
        let _ = CloseHandle(proc);
        if assigned.is_err() {
            let _ = CloseHandle(job);
            return None;
        }
        Some(JobHandle(job))
    }
}

/// A pane's ConPTY master, shared out of the global map (#719). See
/// [`PtyHandle::writer`] for why these two are `Arc<Mutex<..>>` rather than
/// plain fields.
pub type SharedMaster = Arc<Mutex<Box<dyn MasterPty + Send>>>;
/// A pane's stdin writer, shared out of the global map (#719).
pub type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// The completion channel a [`WriterJob`] carries back to its caller
/// (#1607, epic #1600 Phase 2.3). A `tauri::async_runtime::channel(1)` used as
/// a oneshot — one job, one reply, never reused — which is why the capacity is
/// 1 and why the writer thread can always `try_send` without blocking. It is
/// tokio's mpsc under the re-export, so it costs no manifest line.
type WriteReply = tauri::async_runtime::Sender<Result<(), String>>;

/// The completion channel's receiving end, as returned by
/// [`PtyManager::enqueue_frontend_write`] / [`PtyManager::enqueue_cd`].
pub type WriteReceiver = tauri::async_runtime::Receiver<Result<(), String>>;

/// One unit of work for a pane's own writer thread (#1607).
///
/// Both variants carry a `reply`, and that is the whole reason this is not the
/// fire-and-forget queue #719 declined: the command still resolves only when
/// the bytes are actually out, so `src/ptywrite.ts`'s one-in-flight-per-pane
/// chain remains the ordering guarantee (#65) and P6's back pressure is
/// unchanged. See `doc/design/pty-input-path.md` § "719 revisited on isolation".
enum WriterJob {
    /// A keystroke or paste from the frontend — `write_from_frontend`'s body.
    Frontend { data: String, human: bool, reply: WriteReply },
    /// The folder picker's `cd` — `write_cd`'s body.
    Cd { path: String, reply: WriteReply },
}

pub struct PtyHandle {
    /// #719: shared, so `resize_pty`/`size` can take the ConPTY call out of
    /// the global `ptys` lock's scope — clone the `Arc`, drop the map guard,
    /// then work. Per-pane, so one pane's resize serializes only against
    /// itself.
    master: SharedMaster,
    /// #719: shared for the same reason as `master`, and for the sharper one
    /// that motivated the issue. `write_all` into ConPTY's small input pipe
    /// blocks for as long as the child declines to drain it — unbounded, in
    /// the case the issue is about (a busy agent). Doing that while holding
    /// the global `ptys` map lock converted "one agent is busy" into "every
    /// pty command in the app is queued": the map lock is what `write_pty`,
    /// `resize_pty`, `get_output`, the attention scan and pane teardown all
    /// contend on. Cloning the `Arc` out under the map lock and writing under
    /// this per-pane lock keeps the blocking exactly where it belongs — on
    /// the one pane whose pipe is full.
    writer: SharedWriter,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Rolling tail of raw output, teed off the reader thread.
    output: Arc<Mutex<OutputBuf>>,
    /// Unix-ms of the last HUMAN keystroke (write_pty from the frontend);
    /// orchestration's write_bytes does not touch it. Lets prompt delivery
    /// avoid blind-submitting text a human is mid-typing.
    user_input_ms: Arc<std::sync::atomic::AtomicU64>,
    /// Windows-only kill-on-close Job Object owning this pane's process
    /// subtree (issue #78). Dropped when the handle leaves the map (kill,
    /// exit, `kill_all`), which closes the job and reaps every descendant.
    /// `None` when job creation failed (fail-soft) — pre-#78 behavior.
    #[cfg(target_os = "windows")]
    _job: Option<JobHandle>,
    /// How many characters are currently sitting UNSUBMITTED in this pane's
    /// input box (#111, refined by #171). Tracked from the *content* of each
    /// human write, not from output bytes: printable input adds to the count,
    /// backspace/DEL removes from it (clamped at zero), and an Enter /
    /// line-clear resets it straight to zero. `input_pending()` reads this as
    /// `> 0`. Counting rather than a bare flag is what lets a typed-then-fully-
    /// backspaced line read back to empty — a flag set true by the first
    /// keystroke has no signal short of Submit to ever flip back, which used to
    /// leave prompt delivery holding a paste off an already-empty box for the
    /// full 60s abort (#171). This positive submit/clear signal is what lets
    /// prompt delivery hold a paste off a human's half-written line without
    /// wedging on an already-submitted one — output-byte heuristics can't tell
    /// a keystroke's echo from a submit burst.
    input_box_len: Arc<std::sync::atomic::AtomicI64>,
    /// The interactive shell this pane *effectively* spawned (#194 P2) — after
    /// any discovery-miss fallback, not the requested kind. Recorded so the
    /// folder-picker `cd` (`change_dir`) emits the pane's own shell syntax — cmd,
    /// PowerShell, or Git Bash — instead of guessing from the machine default
    /// (rev-78 #3, nit 3). Agent/custom panes record PowerShell (or its cmd
    /// degrade); they don't drive the folder picker.
    shell_kind: ShellKind,
    /// This pane's OWN writer thread (#1607, epic #1600 Phase 2.3) — the
    /// isolation half of the input path.
    ///
    /// #734 put the whole of `write_pty`'s body on `spawn_blocking`, which is
    /// the SHARED 512-slot blocking pool that every converted `orch_*` command
    /// also uses. beta6 exhausted it: polled commands parked on a registry
    /// mutex accumulated there until `write_pty` could no longer be scheduled
    /// at all, so every pane stopped accepting input at once while the window
    /// kept painting (#1600 §1.2). The input path must not compete for threads
    /// with orchestration polling, so it no longer uses that pool: the job goes
    /// to a thread that belongs to this pane and nothing else.
    ///
    /// A wedged pane parks its OWN thread and nothing else's — the same
    /// bound the pool version had (one parked thread per wedged pane, capped
    /// by the pane count), minus the shared resource. The thread exits when
    /// the last `Sender` drops, which is when this handle leaves the map
    /// (pane close, kill, `kill_all`, or the waiter thread's reap). A thread
    /// parked inside a wedged `write_all` exits a moment later instead, when
    /// the ConPTY handle drops and the write errors — exactly the lifetime the
    /// parked pool thread had.
    writer_jobs: std::sync::mpsc::Sender<WriterJob>,
}

/// Ring of recent output plus a monotonic byte counter. The counter lets
/// orchestration detect "did the CLI echo anything since X?" even when the
/// ring is saturated at its cap (where lengths stop changing).
#[derive(Default)]
pub struct OutputBuf {
    ring: VecDeque<u8>,
    total: u64,
}

#[derive(Default)]
pub struct PtyManager {
    ptys: Arc<Mutex<HashMap<u32, PtyHandle>>>,
    next_id: AtomicU32,
    /// Ptys we killed on purpose (pane close, kill_agent): their exit is
    /// "expected", so the frontend closes the pane instead of keeping it
    /// open to display an error.
    expected_exits: Arc<Mutex<HashSet<u32>>>,
    /// Per-pane (gated-writes-since-last-emit, last-emit-unix-ms — `None`
    /// meaning never emitted for this pane) for the throttled
    /// `phantom-input-gated` breadcrumb (#496 N1) — see
    /// `record_phantom_gate`/`phantom_gate_tick`. Cleaned up alongside
    /// `expected_exits` when a pty's waiter thread reaps it.
    neutral_gate_throttle: Arc<Mutex<HashMap<u32, (u64, Option<u64>)>>>,
}

/// The two pieces of [`PtyManager`] a write into a pane's stdin touches: the
/// map (for the pane's writer, its human-input atomics and its shell kind) and
/// the `phantom-input-gated` throttle.
///
/// It exists so the frontend-write body has exactly ONE implementation and two
/// callers (#1607): `PtyManager`'s own methods, which every existing caller and
/// `tests/ptywrite.rs` still go through unchanged, and the pane's writer thread,
/// which cannot hold a `&PtyManager` — in production the manager is Tauri
/// managed state, not an `Arc`, and even if it were, a thread holding the map
/// strongly could never be freed by the map dropping it (the `Sender` that ends
/// the thread lives INSIDE the map). Hence `Weak` on the thread's side and a
/// borrow on the manager's, meeting here.
struct WriteCtx<'a> {
    ptys: &'a Mutex<HashMap<u32, PtyHandle>>,
    neutral_gate_throttle: &'a Mutex<HashMap<u32, (u64, Option<u64>)>>,
}

/// Start a pane's writer thread and return the `Sender` its [`PtyHandle`] keeps
/// (#1607). Called from `spawn_pty_blocking` beside the reader thread, and from
/// `register_fake_inner`, so the `ptywrite.rs`/`liveness.rs` wedge harness
/// covers the shipped path rather than a test-only variant of it.
///
/// `ptys` is WEAK on purpose — see [`WriteCtx`]. An upgrade that fails means the
/// manager itself is gone, which is the same condition as "pty not found" and is
/// reported as such rather than panicking. The upgrade holds a strong reference
/// for the duration of ONE job, so a manager dropped while a write is parked
/// keeps its map alive until that write returns — the same window a direct
/// caller's `&PtyManager` already held open, not a new one.
///
/// Where "pty not found" comes from moves, and the error a caller sees does not:
/// the lookup that produces it is now `PtyManager::enqueue`'s, before the job is
/// posted, rather than `writer_handle`'s inside the body. A pane reaped in
/// between still gets the same `Err` — the job runs, the map no longer holds the
/// pane, and the body returns it from where it always did.
///
/// The loop is a plain `recv()`, so it exits when the last `Sender` drops AND
/// the queue has drained: a job already enqueued when the pane closed still gets
/// run and still gets its reply, instead of leaving an awaiting `write_pty`
/// hanging on a channel nobody will ever answer.
fn spawn_pane_writer(
    id: u32,
    ptys: std::sync::Weak<Mutex<HashMap<u32, PtyHandle>>>,
    neutral_gate_throttle: Arc<Mutex<HashMap<u32, (u64, Option<u64>)>>>,
) -> std::sync::mpsc::Sender<WriterJob> {
    let (tx, rx) = std::sync::mpsc::channel::<WriterJob>();
    std::thread::spawn(move || {
        // Every arm answers its job's `reply` exactly once. `try_send` cannot
        // be full (capacity 1, one reply per job); `Closed` means the awaiting
        // command's future was dropped — the window went away mid-write — and
        // there is simply nobody left to tell.
        while let Ok(job) = rx.recv() {
            let Some(map) = ptys.upgrade() else {
                let (WriterJob::Frontend { reply, .. } | WriterJob::Cd { reply, .. }) = job;
                let _ = reply.try_send(Err("pty not found".to_string()));
                continue;
            };
            let ctx = WriteCtx { ptys: &map, neutral_gate_throttle: &neutral_gate_throttle };
            match job {
                WriterJob::Frontend { data, human, reply } => {
                    let _ = reply.try_send(ctx.write_from_frontend(id, &data, human));
                }
                WriterJob::Cd { path, reply } => {
                    let _ = reply.try_send(ctx.write_cd(id, &path));
                }
            }
        }
    });
    tx
}

impl PtyManager {
    /// Kill every child process; used on app shutdown so shells (and any
    /// agents running in them) don't outlive the window.
    pub fn kill_all(&self) {
        let handles: Vec<_> = self.ptys.lock_safe().drain().collect();
        for (_, mut h) in handles {
            let _ = h.killer.kill();
        }
    }

    /// This pane's stdin writer, cloned out of the global map (#719). The map
    /// lock is held for a hashmap lookup and one `Arc` clone and is released
    /// before the caller touches the pipe, so no IO ever happens inside it.
    ///
    /// Lock order, and the reason there is no deadlock to find here: the
    /// writer lock is a LEAF. Nothing takes the `ptys` map lock (or the output
    /// ring's, or `neutral_gate_throttle`'s) while holding it, and every
    /// acquisition of it in this file follows this method — map lock taken,
    /// released, then writer lock. The reader thread and the attention scan
    /// (#717/#725) never touch the writer at all.
    fn writer_handle(&self, id: u32) -> Option<SharedWriter> {
        self.write_ctx().writer_handle(id)
    }

    /// The borrow of this manager a write body runs against (#1607). Free —
    /// two reference copies — and it is what lets the pane's writer thread and
    /// every direct caller share one implementation. See [`WriteCtx`].
    fn write_ctx(&self) -> WriteCtx<'_> {
        WriteCtx { ptys: &self.ptys, neutral_gate_throttle: &self.neutral_gate_throttle }
    }

    /// This pane's ConPTY master, cloned out of the global map — same contract
    /// as [`PtyManager::writer_handle`], for the resize/geometry calls.
    fn master_handle(&self, id: u32) -> Option<SharedMaster> {
        Some(self.ptys.lock_safe().get(&id)?.master.clone())
    }

    /// Raw write into a pty's stdin; used by orchestration to type prompts
    /// into agent CLIs so the human sees them verbatim.
    ///
    /// #719: still synchronous-completion, deliberately. `Ok` here has always
    /// meant "these bytes went out", and two callers depend on exactly that:
    /// `deliver_prompt` records its own delivered text only once this returns
    /// `Ok` (see `OrchRegistry::record_delivered_text` — a record of text that
    /// never reached the pane is the fail-OPEN direction for the question
    /// gate's mask), and the echo-verified typing loop measures the child's
    /// output from *after* the paste is out. Buffering this write into a queue
    /// and returning early would quietly turn both into claims about bytes
    /// that might never be written. What #719 changes is only where the
    /// blocking happens: on this pane's own writer lock, on the caller's own
    /// (orchestration background) thread, instead of on the global map lock.
    ///
    /// #1607 leaves this one alone for the same reason: it does not go through
    /// the pane's writer thread. Its caller is an orchestration BACKGROUND
    /// thread, never the frontend's blocking pool, so it was never the path
    /// beta6 starved — and routing it through the writer queue would put
    /// orchestration's typing behind a human's keystrokes (and vice versa) for
    /// no gain, while `Ok` would start meaning "the writer thread said so"
    /// instead of "this thread wrote the bytes".
    pub fn write_bytes(&self, id: u32, bytes: &[u8]) -> Result<(), String> {
        self.write_ctx().write_bytes(id, bytes)
    }

    /// Hand a frontend keystroke to this pane's OWN writer thread and return
    /// the completion channel (#1607, epic #1600 Phase 2.3).
    ///
    /// **Everything this does is in memory** — a hashmap lookup, a `Sender`
    /// clone, a channel allocation and a `send` into an unbounded queue — which
    /// is what makes it legal for `write_pty` to run it BEFORE its first await.
    /// Tauri polls an async command's future on the webview thread up to that
    /// first real await (the #716/#724 finding, performance.md §1), so anything
    /// here that could block would be blocking the thread that paints. Nothing
    /// here can: no IO, no leaf lock, no unbounded wait. The map lock is held
    /// for the lookup and the clone, exactly as `writer_handle` holds it.
    ///
    /// The awaited reply is what preserves the two properties #719 refused to
    /// give up. Ordering: the command still resolves only once the bytes are
    /// out, so `src/ptywrite.ts`'s one-in-flight-per-pane chain is still the
    /// thing that orders a pane's own writes (#65). Back pressure: a pane whose
    /// child has stopped draining stops resolving, so the frontend chain stops
    /// dispatching and the unsent remainder waits in the pane's own JS queue —
    /// nothing accumulates backend-side (P6). What changes is only WHICH thread
    /// parks: one that belongs to this pane, never a slot in the 512-thread
    /// pool shared with orchestration polling.
    pub fn enqueue_frontend_write(
        &self,
        id: u32,
        data: String,
        human: bool,
    ) -> Result<WriteReceiver, String> {
        let (reply, rx) = tauri::async_runtime::channel(1);
        // SCRATCH ROUND ONLY (#1607 red-1) — do not merge. The ONE behaviour
        // set aside: the frontend write is routed back through the SHARED
        // blocking pool, which is exactly what `write_pty` did before this PR
        // (#734's `spawn_blocking(move || state.write_from_frontend(..))`).
        // The seam, the reply channel, the writer thread and every other
        // caller are left wired, so the only thing that moves is the
        // destination. tests/liveness.rs L3a must now fail by timeout.
        let ptys = self.ptys.clone();
        let throttle = self.neutral_gate_throttle.clone();
        if !self.ptys.lock_safe().contains_key(&id) {
            return Err("pty not found".to_string());
        }
        tauri::async_runtime::spawn_blocking(move || {
            let ctx = WriteCtx { ptys: &*ptys, neutral_gate_throttle: &*throttle };
            let _ = reply.try_send(ctx.write_from_frontend(id, &data, human));
        });
        Ok(rx)
    }

    /// The folder picker's `cd`, through the same pane writer as a keystroke
    /// (#1607). Same in-memory contract as [`PtyManager::enqueue_frontend_write`].
    ///
    /// This makes `cd`-vs-keystroke on one pane ordered **by arrival** again —
    /// strictly better than the "either order" #719 accepted when both bodies
    /// went to a shared pool, and the one ordering property this change adds
    /// rather than preserves.
    pub fn enqueue_cd(&self, id: u32, path: String) -> Result<WriteReceiver, String> {
        let (reply, rx) = tauri::async_runtime::channel(1);
        self.enqueue(id, WriterJob::Cd { path, reply })?;
        Ok(rx)
    }

    /// Post one job to a pane's writer thread. `pty not found` covers both a
    /// pane that was never registered and one already reaped; a send failure
    /// means the thread is gone while the handle somehow is not, which is not a
    /// state this file can produce — reported rather than unwrapped.
    fn enqueue(&self, id: u32, job: WriterJob) -> Result<(), String> {
        let jobs = {
            let ptys = self.ptys.lock_safe();
            ptys.get(&id).ok_or("pty not found")?.writer_jobs.clone()
        };
        jobs.send(job).map_err(|_| "pty writer gone".to_string())
    }

    /// The whole body of the `write_pty` command (#719), minus the Tauri
    /// wrapper — extracted so an integration test can drive exactly what a
    /// frontend keystroke runs against a real (fake) pty with no `AppHandle`,
    /// the same seam and the same reason as `note_user_input` (#496 PR-A).
    ///
    /// **Ordering: the human-input signal is recorded BEFORE the bytes go
    /// out, and that is the load-bearing direction.** `note_user_input`'s
    /// stamp and box-occupancy counter are what the question gate, the
    /// stranded-text flush and the autonomous idle tick read to answer "is a
    /// human mid-typing in this pane right now?" (#111, #171, #496, #518).
    /// Recording first means the answer is "yes" for the whole window in
    /// which the keystroke is in flight — including the pathological window
    /// this issue is about, where the child is not draining and the write
    /// parks for seconds. Recording after the write would leave that entire
    /// window reading "nothing typed", which is precisely the clobber #111
    /// exists to prevent: a delivery would paste over a line the human has
    /// already committed to. The reverse error — believing a human typed
    /// slightly before their bytes land — only ever makes loomux hold MORE,
    /// which is the fail-safe direction every one of those readers is written
    /// for. This is also exactly the order the pre-#719 command used, so
    /// nothing downstream moves.
    ///
    /// #1607: this is now the body the pane's own WRITER THREAD runs, reached
    /// through [`PtyManager::enqueue_frontend_write`] rather than through the
    /// shared blocking pool. Not one line of it moves — including the order
    /// argued above — and it stays callable directly, which is what keeps
    /// `tests/ptywrite.rs` driving exactly what a keystroke runs.
    pub fn write_from_frontend(&self, id: u32, data: &str, human: bool) -> Result<(), String> {
        self.write_ctx().write_from_frontend(id, data, human)
    }

    /// The body of the `change_dir` command (#719): format the `cd` for this
    /// pane's OWN shell and write it, with the global map lock released before
    /// the write. Same extraction rationale as `write_from_frontend` — and
    /// #1607 routes it the same way, through the pane's own writer thread
    /// ([`PtyManager::enqueue_cd`]), so it is this body the thread runs.
    pub fn write_cd(&self, id: u32, path: &str) -> Result<(), String> {
        self.write_ctx().write_cd(id, path)
    }

    /// Record one FRONTEND-originated write for the human-input signals
    /// (#111 box occupancy, `last_user_input_ms`) — extracted out of
    /// `write_pty` so an integration test can drive it directly against a
    /// real (fake) pty with no Tauri `AppHandle` (#496 PR-A). No-op if the
    /// pty is already gone (a write can race a pane's own teardown).
    ///
    /// `user_input_ms` is stamped only when this write is evidence of an
    /// actual keystroke — `classify_human_input` reads `Content`/`Submit`,
    /// or (Neutral but) a nonzero `box_occupancy_delta` (backspace/DEL).
    /// Before this gate the stamp was unconditional, and that was the bug
    /// (#496): xterm answers a program's terminal queries — OSC colour
    /// queries, DA, DSR/CPR, focus-in/out reports — through this exact same
    /// write path with **no human present at all** (#179's boot-time
    /// instance; copilot also emits these mid-session on redraw/focus
    /// churn). Those replies classify `Neutral` with a zero delta — nothing
    /// typed, nothing removed — so gating the stamp on keystroke evidence
    /// means an auto-reply no longer refreshes `user_input_ms`. That one
    /// unconditional stamp was one root cause behind four symptoms: it
    /// deferred the autonomous idle tick's quiet clock forever, suppressed
    /// the stranded-text flush and submit retries, and withheld Tier-1 box
    /// confirmation — all four read this same timestamp.
    ///
    /// Tradeoff, taken deliberately: pure-`Neutral`-with-zero-delta human
    /// input no longer defers the tick or the flush either — and that class
    /// is broader than "arrow keys, menu navigation" (#496 N2 review): Tab,
    /// Home/End/F-keys, Ctrl-A/E/W/K, and mouse-tracking/wheel CSI reports
    /// (if a TUI enables mouse tracking) all classify `Neutral` with a zero
    /// occupancy delta too — including a human wheel-scrolling a pane while
    /// merely *reading* output, not steering it. This is safe because the
    /// protection for "human mid-menu" already lives in the STATE-based
    /// interactive-question guard (#420 rev-19 R1), which deliberately
    /// dropped keystroke-recency from its own release decision for this
    /// exact reason — the tick is a notice, not an action, and that guard is
    /// untouched by this change.
    ///
    /// Assumption this relies on (#496 N3 review): the classifier is
    /// stateless PER WRITE, so a query reply fragmented across two `write_pty`
    /// calls would have its tail half read in isolation and could stamp (the
    /// escape lead byte that makes the whole sequence read `Neutral` lives in
    /// the first fragment). Unreached today — xterm synthesizes a query
    /// reply as one `onData` event, and the frontend's writer only splits
    /// writes over `PTY_WRITE_CHUNK` (16 KiB), far above any auto-reply's
    /// size — and even if it happened, it fails toward the OLD (safe)
    /// behaviour for that one write, not toward the bug this PR fixes. Still
    /// written down here so a future CLI reply pattern that defeats it is
    /// something the next reader can find, not rediscover.
    ///
    /// **#518: `human_origin` is the structural half of that gate.** Everything
    /// above classifies by BYTE SHAPE, which is a pattern match against an
    /// OPEN set: it covers the auto-reply shapes #179 catalogued and any
    /// other well-formed CSI/OSC/DCS, but #496's own plan §7 closed with
    /// "which copilot emission recurs mid-session" still unanswered — that is
    /// why `record_phantom_gate`'s breadcrumb exists. #518 is the residue of
    /// that open set firing in production: a copilot orchestrator's prompt
    /// sat unsubmitted for want of a keystroke nobody made.
    ///
    /// The frontend already solved this exact problem once, for its own
    /// `firstInputMs`, and NOT by filtering `onData` (#440 B2-R): it takes the
    /// signal from `term.onKey` and the deliberate `term.paste()` sites, which
    /// fire only for genuine keyboard/paste events and are unreachable by
    /// anything the terminal manufactures on its own — "a structural
    /// guarantee, not a pattern match against an open set of possible
    /// auto-reply shapes". `human_origin` carries that same already-proven bit
    /// across the IPC boundary so this stamp can use it too. The two
    /// conditions are ANDed, not swapped: byte shape still has to agree, so a
    /// genuine keystroke that carries no content (an arrow key) is excluded
    /// exactly as #496 PR-A left it.
    ///
    /// `write_pty` defaults it to `true` when a caller says nothing, so an
    /// unstated origin behaves exactly as it did before #518 — the fail-safe
    /// direction, since believing a human typed only ever makes loomux hold
    /// MORE.
    ///
    /// Occupancy (`input_box_len`) is deliberately NOT gated on this. A
    /// terminal auto-reply already contributes a zero delta, so there is
    /// nothing here for the origin bit to add — and under-counting occupancy
    /// is the one direction `box_occupancy_delta`'s doc commits to never
    /// taking, because reading an occupied box as empty is the clobber #111
    /// exists to prevent. #518's bound (`human_input_block`) reads
    /// `input_pending` as its positive evidence, so that counter must stay
    /// governed by the conservative rule alone.
    pub fn note_user_input(&self, id: u32, data: &str, human_origin: bool) {
        self.write_ctx().note_user_input(id, data, human_origin)
    }
}

impl WriteCtx<'_> {
    /// See [`PtyManager::writer_handle`] — this is its body.
    fn writer_handle(&self, id: u32) -> Option<SharedWriter> {
        Some(self.ptys.lock_safe().get(&id)?.writer.clone())
    }

    /// See [`PtyManager::write_bytes`] — this is its body.
    fn write_bytes(&self, id: u32, bytes: &[u8]) -> Result<(), String> {
        let writer = self.writer_handle(id).ok_or("pty not found")?;
        let mut w = writer.lock_safe();
        w.write_all(bytes).map_err(|e| e.to_string())
    }

    /// See [`PtyManager::write_from_frontend`] — this is its body, and the one
    /// a pane's writer thread runs (#1607).
    fn write_from_frontend(&self, id: u32, data: &str, human: bool) -> Result<(), String> {
        self.note_user_input(id, data, human);
        self.write_bytes(id, data.as_bytes())
    }

    /// See [`PtyManager::write_cd`] — this is its body.
    fn write_cd(&self, id: u32, path: &str) -> Result<(), String> {
        let (writer, kind) = {
            let ptys = self.ptys.lock_safe();
            let pty = ptys.get(&id).ok_or("pty not found")?;
            (pty.writer.clone(), pty.shell_kind)
        };
        let line = cd_command_line(path, kind);
        let mut w = writer.lock_safe();
        w.write_all(line.as_bytes()).map_err(|e| e.to_string())
    }

    /// See [`PtyManager::note_user_input`] — this is its body, doc and all.
    fn note_user_input(&self, id: u32, data: &str, human_origin: bool) {
        let classification = crate::orchestration::classify_human_input(data);
        let delta = crate::orchestration::box_occupancy_delta(data);
        let keystroke_like = human_origin
            && (classification != crate::orchestration::HumanInput::Neutral || delta != 0);
        {
            let mut ptys = self.ptys.lock_safe();
            let Some(pty) = ptys.get_mut(&id) else { return };
            if keystroke_like {
                pty.user_input_ms.store(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0),
                    Ordering::Relaxed,
                );
            }
            // Track box occupancy from the keystroke's CONTENT (#111): an Enter /
            // line-clear empties the box outright; everything else (typed text,
            // pastes, backspace/DEL) adjusts a running character count instead of
            // a bare flag, so a line backspaced all the way back out reads as
            // empty again rather than staying stuck "pending" until the 60s hold
            // aborts a delivery (#171).
            match classification {
                crate::orchestration::HumanInput::Submit => {
                    pty.input_box_len.store(0, Ordering::Relaxed)
                }
                crate::orchestration::HumanInput::Content
                | crate::orchestration::HumanInput::Neutral => {
                    if delta != 0 {
                        let cur = pty.input_box_len.load(Ordering::Relaxed);
                        pty.input_box_len.store((cur + delta as i64).max(0), Ordering::Relaxed);
                    }
                }
            }
        }
        // Observability for the one live gap this fix can't settle from code
        // alone (#496 plan section 7): which copilot emission recurs
        // mid-session. Throttled (#496 N1 review — see `record_phantom_gate`)
        // and emitted after the lock is released — breadcrumb does file IO
        // and must not extend the hold on the ptys map.
        if !keystroke_like {
            self.record_phantom_gate(id);
        }
    }

    fn record_phantom_gate(&self, id: u32) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let emit = {
            let mut throttle = self.neutral_gate_throttle.lock_safe();
            let state = throttle.get(&id).copied().unwrap_or((0, None));
            let (emit, new_state) = phantom_gate_tick(state, now);
            throttle.insert(id, new_state);
            emit
        };
        if let Some(count) = emit {
            crate::obs::breadcrumb("phantom-input-gated", &format!("id={id} count={count}"));
        }
    }
}

impl PtyManager {
    /// Snapshot of the rolling output tail (raw bytes, ANSI included).
    pub fn output_tail(&self, id: u32) -> Option<Vec<u8>> {
        let ptys = self.ptys.lock_safe();
        let buf = ptys.get(&id)?.output.lock_safe();
        Some(buf.ring.iter().copied().collect())
    }

    /// Bounded snapshot of the rolling output tail (raw bytes, ANSI included):
    /// only the last `max_bytes`, not the whole (up to 256 KB) ring. For a
    /// caller that polls frequently and only cares what's on screen right now
    /// (a live prompt/question is always the last thing painted) — `output_tail`
    /// clones the entire ring every call, which is measurable waste under this
    /// lock at a 250ms poll cadence held for up to two minutes (#420 rev-15 N4).
    /// Reads from the back of the `VecDeque` so the cost is `O(max_bytes)`, not
    /// `O(ring length)`.
    pub fn output_tail_bounded(&self, id: u32, max_bytes: usize) -> Option<Vec<u8>> {
        let ptys = self.ptys.lock_safe();
        let buf = ptys.get(&id)?.output.lock_safe();
        let mut v: Vec<u8> = buf.ring.iter().rev().take(max_bytes).copied().collect();
        v.reverse();
        Some(v)
    }

    /// This pane's current geometry as `(cols, rows)`, straight from the
    /// master (so a resize since open is reflected). `None` if the pty is
    /// gone or the platform refuses the query — the caller then falls back to
    /// the 80x24 ANSI default.
    ///
    /// Added for #520: replaying a pane's raw output onto a composed grid
    /// (`orchestration::termgrid`) only lands text where the human saw it if
    /// the grid is the pane's real width and height.
    pub fn size(&self, id: u32) -> Option<(u16, u16)> {
        // #719: the map lock is released before the ConPTY query, so this
        // orchestration-thread call can no longer be what a pty command waits
        // behind (nor wait behind one itself).
        let master = self.master_handle(id)?;
        let sz = master.lock_safe().get_size().ok()?;
        Some((sz.cols, sz.rows))
    }

    /// Unix-ms of the last human keystroke into this pty (0 = never).
    pub fn last_user_input_ms(&self, id: u32) -> Option<u64> {
        let ptys = self.ptys.lock_safe();
        Some(ptys.get(&id)?.user_input_ms.load(Ordering::Relaxed))
    }

    /// Whether a human's line is currently sitting unsubmitted in this pane's
    /// input box (#111). `None` if the pty is gone. Prompt delivery consults this
    /// before pasting so it never merge-submits a human's half-written line.
    pub fn input_pending(&self, id: u32) -> Option<bool> {
        let ptys = self.ptys.lock_safe();
        Some(ptys.get(&id)?.input_box_len.load(Ordering::Relaxed) > 0)
    }

    /// Monotonic count of bytes this pty has ever produced.
    pub fn output_total(&self, id: u32) -> Option<u64> {
        let ptys = self.ptys.lock_safe();
        let total = ptys.get(&id)?.output.lock_safe().total;
        Some(total)
    }

    /// Ids of every live pty. Lets the attention scan (#40) cover *all* panes —
    /// including plain shells the human opened by hand, which have no
    /// orchestration identity — not just registered agents.
    pub fn live_ids(&self) -> Vec<u32> {
        self.ptys.lock_safe().keys().copied().collect()
    }

    /// Kill one child; the waiter thread reaps it and emits `pty-exit`.
    ///
    /// **The `let` on the `remove` line is load-bearing, not a style choice**
    /// (#743 S7, INV-5). A temporary `MutexGuard` in a `let` initializer is
    /// dropped at the end of *that statement*, so the global map lock is
    /// released the instant the handle is out — and `killer.kill()`, a
    /// `TerminateProcess`/`kill(2)` syscall that can block on a wedged child,
    /// then runs holding nothing. Rewriting this as a bound guard
    /// (`let mut ptys = self.ptys.lock_safe(); let h = ptys.remove(&id);`)
    /// looks identical and is not: it extends the lock over the syscall,
    /// which is exactly what `writer_handle`'s stated lock order forbids.
    /// `kill_all` above has the same shape for the same reason.
    pub fn kill(&self, id: u32) {
        self.expected_exits.lock_safe().insert(id);
        let handle = self.ptys.lock_safe().remove(&id);
        if let Some(mut h) = handle {
            let _ = h.killer.kill();
        }
    }

    /// Test-only harness (#420 rev-19 R3): register a pty backed by a REAL
    /// ConPTY pair + a trivial spawned child (so `master`/`killer` are
    /// genuine, matching production shape exactly) but whose OUTPUT ring is
    /// manually seeded and whose writes are captured into a plain `Vec<u8>`
    /// instead of the child's stdin — deterministic, inspectable content with
    /// no dependency on any real shell's own rendering or timing. This is
    /// what lets an integration test drive the ACTUAL `PtyManager` methods
    /// (`write_bytes`, `output_tail`/`output_tail_bounded`, `output_total`,
    /// ...) that orchestration code (e.g. `deliver_prompt`'s stranded-text
    /// flush) calls, without a real Tauri `AppHandle` — unavailable headless,
    /// since `tauri::test`'s `MockRuntime` isn't the concrete `Wry` runtime
    /// production `spawn_pty` requires — and without a real agent CLI
    /// (CLAUDE.md constraint 3: the trivial child is a bare shell that does
    /// nothing, never an agent). Returns the shared write-capture buffer.
    ///
    /// rev-19 B-B: the child MUST NOT be able to outlive this handle. Two
    /// independent layers, matching production `spawn_pty` exactly rather
    /// than inventing a test-only shortcut: (1) the spawned command exits
    /// immediately on its own (`cmd /c exit 0` / `sh -c true`, not a
    /// wait-for-input command like the original `pause>nul`, which blocked
    /// forever and was the actual leak — nothing in this harness needs the
    /// child to stay alive, since `write_bytes`/`output_tail`/`output_total`
    /// never touch it, only `master`/`killer`, which just need to be REAL
    /// objects to satisfy `PtyHandle`'s fields); (2) on Windows, the SAME
    /// kill-on-close Job Object (#78, `assign_kill_on_close_job`) production
    /// spawns use is assigned here too — `_job: None` (the original cut)
    /// opted every fake OUT of that safety net, so a test panic mid-run (no
    /// `Drop` reached, or the process itself killed) could still orphan the
    /// child. With the job assigned, closing the LAST handle to it (this
    /// `PtyHandle` being dropped, however that happens) kills the subtree at
    /// the kernel level — structural, not dependent on any test's own
    /// cleanup code running.
    #[doc(hidden)] // pub for integration tests
    pub fn register_fake_for_test(&self, id: u32, initial_output: &[u8]) -> Arc<Mutex<Vec<u8>>> {
        self.register_fake_inner(id, initial_output, None)
    }

    /// Test-only (#719): a fake pty whose writer PARKS inside `write` until
    /// the returned [`PtyWriteGate`] is opened. That is the hazard this issue
    /// is about — a child that has stopped draining its stdin pipe, so
    /// `write_all` does not return — reproduced deterministically, with no
    /// sleeps, no real agent CLI (CLAUDE.md constraint 3), and no dependence
    /// on how full a real ConPTY input pipe happens to be on the host.
    ///
    /// The gate is entered BEFORE the bytes are captured, so a parked write is
    /// observably "in the pipe but not through it": a test can assert both
    /// that the caller has not returned and that nothing has landed yet.
    #[doc(hidden)] // pub for integration tests
    pub fn register_gated_fake_for_test(
        &self,
        id: u32,
    ) -> (Arc<Mutex<Vec<u8>>>, Arc<PtyWriteGate>) {
        let gate = Arc::new(PtyWriteGate::default());
        let captured = self.register_fake_inner(id, b"", Some(gate.clone()));
        (captured, gate)
    }

    fn register_fake_inner(
        &self,
        id: u32,
        initial_output: &[u8],
        gate: Option<Arc<PtyWriteGate>>,
    ) -> Arc<Mutex<Vec<u8>>> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .expect("openpty");
        let mut cmd = if cfg!(windows) {
            let mut c = CommandBuilder::new("cmd.exe");
            c.args(["/c", "exit", "0"]);
            c
        } else {
            let mut c = CommandBuilder::new("sh");
            c.args(["-c", "true"]);
            c
        };
        cmd.cwd(std::env::temp_dir());
        let child = pair.slave.spawn_command(cmd).expect("spawn trivial child for fake pty");
        drop(pair.slave);
        let killer = child.clone_killer();

        #[cfg(target_os = "windows")]
        let job = match child.process_id() {
            Some(pid) => assign_kill_on_close_job(pid),
            None => None,
        };

        struct CaptureWriter(Arc<Mutex<Vec<u8>>>, Option<Arc<PtyWriteGate>>);
        impl Write for CaptureWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                // Park first, capture second: a gated write is then observably
                // stuck *before* its bytes land, which is what makes "the
                // caller has not returned AND nothing was written" assertable.
                if let Some(gate) = &self.1 {
                    gate.enter();
                }
                self.0.lock_safe().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        let ring: VecDeque<u8> = initial_output.iter().copied().collect();
        let output = Arc::new(Mutex::new(OutputBuf { ring, total: initial_output.len() as u64 }));

        // #1607: a fake pane gets the SAME writer thread a real one does, so
        // `tests/ptywrite.rs`'s and `tests/liveness.rs`'s wedge harness
        // exercises the shipped input path rather than a test-only variant of
        // it — the whole point of the harness being that a wedged pane is
        // reproduced deterministically.
        let writer_jobs = spawn_pane_writer(
            id,
            Arc::downgrade(&self.ptys),
            self.neutral_gate_throttle.clone(),
        );
        self.ptys.lock_safe().insert(
            id,
            PtyHandle {
                master: Arc::new(Mutex::new(pair.master)),
                writer: Arc::new(Mutex::new(
                    Box::new(CaptureWriter(captured.clone(), gate)) as Box<dyn Write + Send>
                )),
                killer,
                output,
                user_input_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                #[cfg(target_os = "windows")]
                _job: job,
                input_box_len: Arc::new(std::sync::atomic::AtomicI64::new(0)),
                shell_kind: ShellKind::PowerShell,
                writer_jobs,
            },
        );
        captured
    }

    /// Test-only: append `bytes` to a registered pty's output ring exactly
    /// as the real reader thread does — bump `total`, extend the ring, drain
    /// the overflow past `OUTPUT_RING_CAP`. `register_fake_for_test` seeds a
    /// ring once and there is no reader thread behind a fake, so this is the
    /// only way a test can exercise what orchestration sees as a pane KEEPS
    /// producing output — in particular the saturated-ring case (#517),
    /// where the ring's length stops changing while `total` keeps climbing.
    ///
    /// Mirrors the reader's body deliberately rather than sharing it: the
    /// reader also owns blocking IO and a Tauri `emit`, neither of which a
    /// headless test has. Keeping the cap arithmetic identical is the point
    /// — a test whose ring saturated differently from production's would
    /// prove nothing about production.
    #[doc(hidden)] // pub for integration tests
    pub fn append_fake_output_for_test(&self, id: u32, bytes: &[u8]) {
        let ptys = self.ptys.lock_safe();
        let Some(pty) = ptys.get(&id) else { return };
        let mut out = pty.output.lock_safe();
        out.total += bytes.len() as u64;
        out.ring.extend(bytes);
        let overflow = out.ring.len().saturating_sub(OUTPUT_RING_CAP);
        if overflow > 0 {
            out.ring.drain(..overflow);
        }
    }

    /// Back-date this pane's keystroke-recency clock (#518, integration tests
    /// only). `note_user_input` can only ever stamp `now`, so without this
    /// there is no way to reach the one state #518's whole fix is about — a
    /// human-input block standing on evidence that has gone STALE — short of a
    /// test that sleeps for the bound (ten minutes).
    ///
    /// Deliberately a back-date of the REAL field the real readers read, not a
    /// mock or an injected clock: `human_input_block_now`,
    /// `drain_stranded_submit` and the late monitor all keep using production
    /// `now_ms()` and the shipped bound, so what a test drives with this is
    /// the actual delivery path, not a parallel one built for testing.
    #[doc(hidden)] // pub for integration tests
    pub fn set_user_input_ms_for_test(&self, id: u32, ms: u64) {
        let ptys = self.ptys.lock_safe();
        if let Some(pty) = ptys.get(&id) {
            pty.user_input_ms.store(ms, Ordering::Relaxed);
        }
    }

    /// The pane's own output-ring mutex, so an integration test can HOLD it and
    /// make every `output_tail`/`output_tail_bounded` of that pane park inside
    /// the read (#743 S7).
    ///
    /// This is the read-side counterpart of [`PtyWriteGate`], and it exists for
    /// the same reason: a leaf-lock claim ("while this pane's ring is being
    /// read, everything else still moves") is only provable if the read can be
    /// held still deterministically, without sleeps or a race against how fast
    /// a 256 KiB clone happens to run on the host. It hands back the REAL lock
    /// the shipped readers take — no parallel mechanism, so what a test wedges
    /// is what production wedges. Nothing in the product path calls it, and it
    /// adds no field, branch or cost to the read itself.
    #[doc(hidden)] // pub for integration tests
    pub fn output_ring_for_test(&self, id: u32) -> Option<Arc<Mutex<OutputBuf>>> {
        Some(self.ptys.lock_safe().get(&id)?.output.clone())
    }
}

/// Test-only write barrier for [`PtyManager::register_gated_fake_for_test`]
/// (#719). A fake pty's writer calls [`PtyWriteGate::enter`] and parks there
/// until [`PtyWriteGate::open`] releases it, standing in for a child that has
/// stopped draining its stdin pipe.
///
/// Deliberately a rendezvous, not a sleep: the test learns the write is
/// *inside* the writer from [`PtyWriteGate::wait_for_writes`] rather than
/// guessing at a duration, so the assertions that follow ("the pane is wedged
/// — now can anything else still make progress?") are about a state that has
/// actually been reached, on a slow CI runner as much as a fast one.
#[doc(hidden)] // pub for integration tests
#[derive(Default)]
pub struct PtyWriteGate {
    state: Mutex<PtyWriteGateState>,
    cv: std::sync::Condvar,
}

#[derive(Default)]
struct PtyWriteGateState {
    open: bool,
    /// How many writes have reached the barrier over this gate's lifetime.
    /// Counted, not a flag, so a test can wait for the Nth write specifically.
    entered: u32,
}

impl PtyWriteGate {
    /// Called from the fake writer: announce this write and park until open.
    fn enter(&self) {
        let mut st = self.state.lock_safe();
        st.entered += 1;
        self.cv.notify_all();
        while !st.open {
            st = self.cv.wait(st).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Block until at least `n` writes have parked in the writer, or
    /// `timeout` elapses. Returns whether they did.
    pub fn wait_for_writes(&self, n: u32, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut st = self.state.lock_safe();
        while st.entered < n {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now())
            else {
                return false;
            };
            let (guard, _) = self
                .cv
                .wait_timeout(st, remaining)
                .unwrap_or_else(|e| e.into_inner());
            st = guard;
        }
        true
    }

    /// Release every parked write, and every later one.
    pub fn open(&self) {
        self.state.lock_safe().open = true;
        self.cv.notify_all();
    }
}

/// Minimum spacing between `phantom-input-gated` breadcrumbs for the SAME
/// pane (#496 N1 review). Un-throttled, arrow keys/Tab/Ctrl-nav/mouse-
/// tracking wheel reports from a human ACTIVELY using a pane hit
/// `note_user_input`'s gated branch on every single write — tens of
/// file-opens per second while scrolling — and worse, the resulting flood of
/// `id=<n>` lines dilutes and can rotate the mid-session copilot signal this
/// breadcrumb exists to capture (plan §7) out of the 2 MB `breadcrumbs.log`
/// before anyone goes looking, since it shares that log with every other
/// pane's lifecycle events. Restricting emission to panes with an
/// orchestration identity was the preferred alternative (per review), but
/// that needs `note_user_input` (a `pty.rs`-owned, per-write hot path) to
/// reach into orchestration's agent registry — real coupling into a
/// different module's state, not just calling its already-shared pure
/// classifier — so this throttle was chosen instead: it stays inside
/// `pty.rs`, needs no new command wiring, and still bounds the log growth
/// this finding is about. One line per pane per interval, carrying how many
/// gated writes landed since the last line, keeps the *rate* visible without
/// the flood.
const PHANTOM_GATE_BREADCRUMB_MIN_INTERVAL_MS: u64 = 5_000;

/// Pure throttle decision for the `phantom-input-gated` breadcrumb (#496 N1
/// review). `state` is the pane's (gated-writes-since-last-emit,
/// last-emit-unix-ms, or `None` if this pane has never emitted); `now_ms` is
/// the current time. Returns `(Some(count_to_report), new_state)` when this
/// write should emit (the count includes this write, and the counter resets
/// to 0), or `(None, new_state)` when it should just accumulate silently
/// until the interval has passed.
///
/// `last_emit_ms` is `Option`, not a bare `0` sentinel for "never emitted":
/// stamping the pane's very first gated write always emits regardless of
/// what `now_ms` happens to be, which a `0` sentinel only gets right by
/// coincidence (`now_ms.saturating_sub(0)` clears any realistic interval only
/// because real epoch-ms values are astronomically larger than the
/// millisecond-scale interval — true in production, but it silently fails
/// for small/synthetic `now_ms` values, exactly what a unit test needs to
/// use so it isn't tied to wall-clock magnitudes).
///
/// Pulled out to a pure function, separate from `record_phantom_gate`'s
/// locking and `obs::breadcrumb`'s file IO, so the rate-limit itself — the
/// new logic this finding adds — is unit-testable with no filesystem
/// involved at all (#496 N4 review: the breadcrumb's own disk write has no
/// non-racy cross-crate test seam from an integration test — `obs.rs`'s
/// `LOG_DIR_OVERRIDE` is `#[cfg(test)]`-internal to that crate's own unit
/// tests, and mutating `LOOMUX_DATA_DIR` globally would race every other
/// test in the same parallel binary — but this decision has no such
/// dependency and can be driven directly).
pub fn phantom_gate_tick(
    state: (u64, Option<u64>),
    now_ms: u64,
) -> (Option<u64>, (u64, Option<u64>)) {
    let (count, last_emit_ms) = state;
    let count = count + 1;
    let due = match last_emit_ms {
        None => true,
        Some(last) => now_ms.saturating_sub(last) >= PHANTOM_GATE_BREADCRUMB_MIN_INTERVAL_MS,
    };
    if due {
        (Some(count), (0, Some(now_ms)))
    } else {
        (None, (count, last_emit_ms))
    }
}

#[derive(Clone, Serialize)]
struct ExitPayload {
    id: u32,
    exit_code: Option<u32>,
    /// True when loomux itself killed the process (pane close, kill_agent).
    expected: bool,
}

#[derive(Clone, Serialize)]
struct OutputPayload {
    id: u32,
    /// Base64-encoded raw bytes so the transport is lossless.
    data: String,
}

/// PowerShell prompt hook that reports the working directory to the terminal
/// via an OSC 7 sequence on every prompt. This is how we track `cd`s:
/// PowerShell keeps its own logical location and never moves the OS process
/// cwd, so polling the process is useless — the shell has to tell us.
/// Written with single quotes only so it needs no shell-quote escaping.
const PWSH_CWD_HOOK: &str = "$global:__loomuxInner=$function:prompt; \
function global:prompt { \
if ($PWD.Provider.Name -eq 'FileSystem') { \
[Console]::Write([char]27+']7;'+$PWD.ProviderPath+[char]7) }; \
& $global:__loomuxInner }";

/// The interactive shell a Terminal pane asks for (#194 P2). The wire value is
/// the lowercase string the frontend's `ShellKind` sends; an unknown or absent
/// value resolves to PowerShell **explicitly** (see `parse`) — never silently —
/// so a Terminal pane always gets a working shell and a bad caller is visible.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShellKind {
    PowerShell,
    Cmd,
    GitBash,
}

impl ShellKind {
    /// Map the frontend's wire string to a kind. Anything unrecognized —
    /// including `None` (no `shell_kind` passed) — falls back to PowerShell, the
    /// universal Windows default. On the fallback the caller breadcrumbs, so the
    /// mismatch shows up instead of quietly spawning the wrong shell.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("cmd") => ShellKind::Cmd,
            Some("gitbash") => ShellKind::GitBash,
            _ => ShellKind::PowerShell,
        }
    }
}

/// Pick the user's default interactive shell.
fn default_shell() -> String {
    #[cfg(target_os = "windows")]
    {
        // Prefer PowerShell 7 when available, fall back to Windows PowerShell.
        for candidate in ["pwsh.exe", "powershell.exe"] {
            if which(candidate) {
                return candidate.to_string();
            }
        }
        "cmd.exe".to_string()
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
    }
}

#[cfg(target_os = "windows")]
fn which(name: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

/// Resolve a program name to its first PATH hit — a discovery cousin of `which`
/// that returns the path rather than a bool.
#[cfg(target_os = "windows")]
fn which_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Candidate `bash.exe` paths for a Git for Windows install, in preference
/// order: the standard Program Files roots, then a per-user install under
/// LOCALAPPDATA. `bin\bash.exe` is the launcher wrapper the Git Bash shortcut
/// runs (it sets up the MSYS environment), so we prefer it over `usr\bin`.
/// Env-driven so a relocated Program Files still resolves. Pure (only builds
/// paths, touches no filesystem) so the layout logic is unit-testable.
#[cfg(target_os = "windows")]
fn git_bash_candidates() -> Vec<PathBuf> {
    let program_roots: Vec<PathBuf> = ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|var| std::env::var_os(var).map(PathBuf::from))
        .collect();
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    git_bash_candidates_from(&program_roots, local.as_deref())
}

/// Pure core of `git_bash_candidates`: given the Program Files roots (in
/// preference order) and the optional LOCALAPPDATA dir, produce the ordered
/// `bin\bash.exe` candidates. Split out so the precedence is unit-testable
/// against fixed inputs, independent of the machine's environment (rev-78 #5).
#[cfg(target_os = "windows")]
fn git_bash_candidates_from(program_roots: &[PathBuf], localappdata: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = program_roots.iter().map(|r| r.join("Git")).collect();
    if let Some(local) = localappdata {
        roots.push(local.join("Programs").join("Git"));
    }
    roots
        .into_iter()
        .map(|r| r.join("bin").join("bash.exe"))
        .collect()
}

/// Whether a discovered `bash.exe` is a Windows system shell (WSL's launcher),
/// not Git Bash. WSL ships `%SystemRoot%\System32\bash.exe`, which is on PATH on
/// every machine with the feature enabled and would spawn a Linux distro in the
/// pane — never Git for Windows. Pure (path + the provided system root) so the
/// exclusion is unit-testable (rev-78 #2). Case-insensitive to tolerate a
/// relocated / differently-cased Windows install.
#[cfg(target_os = "windows")]
fn is_system_bash(path: &Path, system_root: Option<&Path>) -> bool {
    // Normalize `/`→`\` before comparing so a forward-slash PATH entry
    // (`C:/Windows/System32/bash.exe`) can't evade the check (rev-78 nit 2).
    let norm = path.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    if let Some(root) = system_root {
        let root = root.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
        let root = root.trim_end_matches('\\');
        if !root.is_empty() {
            // Match at a component boundary (`<root>\…`) so `C:\WindowsFoo\…`
            // isn't caught by a `C:\Windows` prefix.
            if norm.starts_with(&format!("{root}\\")) {
                return true;
            }
        }
    }
    // Fallback when SystemRoot is unreadable: the WSL launcher lives in
    // ...\System32\bash.exe, a location Git for Windows never occupies.
    norm.contains("\\system32\\")
}

/// Derive `bash.exe` from a discovered `git.exe`. Git for Windows lays out
/// `<root>\cmd\git.exe` (what lands on PATH) and `<root>\bin\git.exe`; bash lives
/// at `<root>\bin\bash.exe` in both cases. Pure path arithmetic (no filesystem)
/// so it is unit-testable against fixed inputs.
#[cfg(target_os = "windows")]
fn git_exe_to_bash(git_exe: &Path) -> Option<PathBuf> {
    let root = git_exe.parent()?.parent()?;
    Some(root.join("bin").join("bash.exe"))
}

/// Locate `bash.exe` for the Git Bash shell kind: the standard install roots
/// first, then PATH (a direct `bash.exe`, or one derived from `git.exe`, which
/// is on PATH far more often). `None` means Git for Windows isn't installed —
/// the frontend disables the Git Bash option with that reason, and the spawn
/// path falls back to PowerShell rather than crashing the pane (#194 P2).
#[cfg(target_os = "windows")]
fn find_git_bash() -> Option<PathBuf> {
    // 1. Standard Git-for-Windows install roots — the most reliable signal.
    for cand in git_bash_candidates() {
        if cand.is_file() {
            return Some(cand);
        }
    }
    // 2. Derive from git.exe on PATH BEFORE trusting a bare bash.exe: git.exe is
    //    almost always the Git-for-Windows one (scoop/winget portable installs
    //    put only `…\cmd\git.exe` on PATH), whereas a bare `bash.exe` PATH hit is
    //    frequently WSL's System32 launcher (rev-78 #2).
    if let Some(git) = which_path("git.exe") {
        if let Some(bash) = git_exe_to_bash(&git) {
            if bash.is_file() {
                return Some(bash);
            }
        }
    }
    // 3. Last resort: a bare bash.exe on PATH, excluding WSL's System32 launcher
    //    (picking it would spawn a Linux distro in the pane, with our
    //    `--login -i` args and PROMPT_COMMAND never reaching the Linux shell).
    let system_root = std::env::var_os("SystemRoot").map(PathBuf::from);
    if let Some(bash) = which_path("bash.exe") {
        if !is_system_bash(&bash, system_root.as_deref()) {
            return Some(bash);
        }
    }
    None
}

/// `cmd.exe` interactive shell (`/K` keeps it open). The PROMPT string emits an
/// OSC 7 sequence (`$E]7;…$E\`) before the visible `path>` so the pane's
/// dir/branch chip tracks `cd`s — cmd has no prompt-hook mechanism, so its
/// PROMPT is the only place to wire cwd reporting.
#[cfg(target_os = "windows")]
fn cmd_shell_command() -> CommandBuilder {
    let mut cmd = CommandBuilder::new("cmd.exe");
    cmd.args(["/K", "prompt $E]7;$P$E\\$P$G"]);
    cmd
}

/// PowerShell interactive shell (pwsh 7, else Windows PowerShell) with the OSC 7
/// prompt hook. Degrades to `cmd.exe` only when neither PowerShell is present —
/// `default_shell` already encodes that preference order, so this is also the
/// explicit fallback target for an unknown/absent or uninstalled shell kind.
#[cfg(target_os = "windows")]
fn powershell_shell_command() -> CommandBuilder {
    let shell = default_shell();
    if shell.contains("cmd.exe") {
        return cmd_shell_command();
    }
    let mut cmd = CommandBuilder::new(&shell);
    cmd.args(["-NoLogo", "-NoExit", "-Command", PWSH_CWD_HOOK]);
    cmd
}

/// Git Bash interactive shell. Launched as a login+interactive shell
/// (`--login -i`), exactly like the Git Bash shortcut, so the MSYS environment
/// (coreutils on PATH, the MSYS home dir) is set up. OSC 7 cwd reporting is
/// wired via PROMPT_COMMAND — but the payload is run through `cygpath -m` so it
/// emits a Windows-form path (`C:/Projects/x`), NOT MSYS `$PWD` (`/c/...`):
/// `dir_info`, the branch chip, and the git-change watcher are all Windows-path
/// consumers, and a raw MSYS path resolves to nothing (rev-78 #1). `cygpath`
/// ships in every Git-for-Windows `/usr/bin`, on PATH under `--login`; the
/// `2>/dev/null || printf %s` guard keeps a stray shell (no cygpath) from
/// printing a per-prompt error and degrades to the raw `$PWD` (rev-78 nit 1).
#[cfg(target_os = "windows")]
fn git_bash_shell_command(bash: &Path) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(bash.as_os_str());
    cmd.args(["--login", "-i"]);
    cmd.env(
        "PROMPT_COMMAND",
        "printf '\\033]7;%s\\007' \"$(cygpath -m \"$PWD\" 2>/dev/null || printf %s \"$PWD\")\"",
    );
    cmd
}

/// Build the interactive (no-command) shell for a Terminal pane's chosen kind
/// (#194 P2). A Git Bash discovery miss falls back to PowerShell, breadcrumbed
/// so it isn't silent (the frontend also disables an uninstalled Git Bash, so
/// this only fires for a non-UI caller or an install/uninstall race).
#[cfg(target_os = "windows")]
fn interactive_shell_command(kind: ShellKind) -> CommandBuilder {
    match kind {
        ShellKind::PowerShell => powershell_shell_command(),
        ShellKind::Cmd => cmd_shell_command(),
        ShellKind::GitBash => match find_git_bash() {
            Some(bash) => git_bash_shell_command(&bash),
            None => {
                crate::obs::breadcrumb("shell-kind-fallback", "gitbash-not-installed->powershell");
                powershell_shell_command()
            }
        },
    }
}

/// POSIX interactive shell. `shell_kind` is a Windows concept (PowerShell / cmd /
/// Git Bash); off Windows the pane always gets the user's login shell with OSC 7
/// wired via PROMPT_COMMAND.
#[cfg(not(target_os = "windows"))]
fn interactive_shell_command(_kind: ShellKind) -> CommandBuilder {
    let shell = default_shell();
    let mut cmd = CommandBuilder::new(&shell);
    cmd.arg("-l");
    cmd.env("PROMPT_COMMAND", "printf '\\033]7;%s\\007' \"$PWD\"");
    cmd
}

/// The shell kind a pane will *actually* run, resolving the same fallbacks
/// `interactive_shell_command` applies: a Git Bash discovery miss becomes
/// PowerShell, and PowerShell with no pwsh installed becomes cmd. Recorded on
/// the handle (not the *requested* kind) so `change_dir` emits the truthful
/// shell's `cd` syntax even in the probe→spawn discovery-miss race (rev-78 nit 3).
#[cfg(target_os = "windows")]
fn effective_shell_kind(requested: ShellKind) -> ShellKind {
    match requested {
        ShellKind::GitBash if find_git_bash().is_none() => {
            effective_shell_kind(ShellKind::PowerShell)
        }
        ShellKind::PowerShell if default_shell().contains("cmd.exe") => ShellKind::Cmd,
        other => other,
    }
}

#[cfg(not(target_os = "windows"))]
fn effective_shell_kind(requested: ShellKind) -> ShellKind {
    requested
}

/// Discover the Git Bash `bash.exe` path so the welcome screen can enable (or
/// disable, with a reason) the Git Bash shell kind before a pane is spawned
/// (#194 P2). `None` = Git for Windows isn't installed. Always `None` off
/// Windows, where Git Bash isn't a concept.
///
/// Off-thread (#746 — `crate::blocking::run_blocking`, P1 of
/// `doc/design/performance.md`): `find_git_bash` stats a bounded candidate list
/// and scans PATH, on the thread that services paint, at launcher time.
///
/// **Reentrancy.** A pure read of the machine, with no lock and no mutation:
/// two probes stat the same paths and agree. Nothing here is cached, so there
/// is not even a cache for a race to disagree about.
#[tauri::command]
pub async fn discover_git_bash() -> Option<String> {
    crate::blocking::run_blocking(|| {
        #[cfg(target_os = "windows")]
        {
            find_git_bash().map(|p| p.to_string_lossy().into_owned())
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    })
    .await
}

/// Candidate `ssh.exe` paths for the Windows inbox OpenSSH client, used only
/// when PATH has no `ssh.exe` at all (#887 S3). Pure (builds paths, touches no
/// filesystem) so the layout is unit-testable against fixed inputs, like
/// `git_bash_candidates_from`.
///
/// Both entries are the SAME install seen through two names, in the order they
/// should be tried:
///  - `System32\OpenSSH` — where Windows 10 1809+ ships the client.
///  - `Sysnative\OpenSSH` — the same directory as seen from a 32-bit process,
///    for which `System32` is silently redirected to `SysWOW64` (which has no
///    OpenSSH). Costs one extra `is_file` on a path that simply doesn't exist
///    for a 64-bit process, and is the difference between "found" and "not
///    installed" for one that isn't.
///
/// Driven entirely by `SystemRoot` rather than a literal `C:\Windows`: a machine
/// is free to have Windows anywhere, and hardcoding one operator's layout into
/// product code is what CLAUDE.md constraint 8 refuses. No `SystemRoot` at all
/// means no candidates — an empty list, not a guess.
#[cfg(target_os = "windows")]
fn ssh_candidates_from(system_root: Option<&Path>) -> Vec<PathBuf> {
    let Some(root) = system_root else {
        return Vec::new();
    };
    ["System32", "Sysnative"]
        .iter()
        .map(|dir| root.join(dir).join("OpenSSH").join("ssh.exe"))
        .collect()
}

/// Locate the local `ssh.exe` an SSH pane spawns (#887 S3): PATH first, then the
/// inbox OpenSSH install directory.
///
/// PATH wins deliberately — a user who installed a newer OpenSSH, or who puts a
/// wrapper on PATH ahead of the inbox client, has already expressed which ssh
/// they mean, and every other program on the machine honours that. The candidate
/// list below is the fallback for a STRIPPED PATH (a common enough shape in
/// locked-down environments, and the one case where "ssh isn't installed" would
/// otherwise be reported about a machine that ships it).
///
/// `None` means no ssh client was found: the launcher then refuses the launch
/// with that reason instead of spawning a pane that dies on its first line.
#[cfg(target_os = "windows")]
fn find_ssh() -> Option<PathBuf> {
    if let Some(ssh) = which_path("ssh.exe") {
        return Some(ssh);
    }
    let system_root = std::env::var_os("SystemRoot").map(PathBuf::from);
    ssh_candidates_from(system_root.as_deref())
        .into_iter()
        .find(|cand| cand.is_file())
}

/// Resolve the local OpenSSH client for an SSH pane (#887 S3), as an absolute
/// path, or `None` when this machine has none. The launcher probes this before
/// it will build an ssh command line, and hands the resolved path back as the
/// spawned argv's program — resolving ONCE, here, so the path that was probed is
/// the path that runs (a bare `"ssh"` would be re-resolved at spawn time against
/// a different PATH snapshot, and could silently be a different binary, or none).
///
/// Off-thread (#746 — `crate::blocking::run_blocking`, P1 of
/// `doc/design/performance.md`) for the same reason as `discover_git_bash`: it
/// scans PATH and stats a small candidate list, at launcher time, on the thread
/// that services paint. Always `None` off Windows — the pane kind is Windows-only
/// like every other spawn path here.
///
/// **Reentrancy.** A pure read of the machine: no lock, no mutation, no cache for
/// two concurrent probes to disagree about.
#[tauri::command]
pub async fn discover_ssh() -> Option<String> {
    crate::blocking::run_blocking(|| {
        #[cfg(target_os = "windows")]
        {
            find_ssh().map(|p| p.to_string_lossy().into_owned())
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    })
    .await
}

/// Whether the direct-CLI spawn path (issue #78) is disabled by the escape
/// hatch. Set `LOOMUX_NO_DIRECT_SPAWN` to any value other than empty/`0`/`false`
/// to force every agent pane back through the shell wrapper (the pre-#78
/// behavior) — a one-env-var rollback if a direct spawn ever misbehaves.
fn direct_spawn_disabled() -> bool {
    match std::env::var("LOOMUX_NO_DIRECT_SPAWN") {
        Ok(v) => {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        }
        Err(_) => false,
    }
}

/// Try to build a *direct* pane child from a structured `argv` — the resolved
/// agent executable spawned as the ConPTY child with no pwsh/sh wrapper in
/// between (issue #78). Returns `None` (caller falls back to the shell path)
/// when the escape hatch is set, `argv` is empty, the program can't be resolved
/// on PATH, or it resolves to a `.cmd`/`.bat`/`.ps1` shim that `CreateProcess`
/// can't launch directly. Every fallback is breadcrumbed so a lost win is
/// diagnosable.
fn try_direct_command(argv: &[String]) -> Option<CommandBuilder> {
    if direct_spawn_disabled() {
        return None;
    }
    let program = argv.first().map(|p| p.trim()).filter(|p| !p.is_empty())?;
    let path_env = crate::winpath::launch_path();
    let resolved = match crate::winpath::resolve_program(
        program,
        &path_env,
        &crate::winpath::launch_pathext(),
    ) {
        Some(p) => p,
        None => {
            crate::obs::breadcrumb("pty-direct-fallback", &format!("unresolved program={program}"));
            return None;
        }
    };
    if !crate::winpath::is_native_executable(&resolved) {
        // A shim (.cmd/.ps1) needs a shell interpreter — keep the wrapper.
        crate::obs::breadcrumb("pty-direct-fallback", &format!("shim program={program}"));
        return None;
    }
    let mut cmd = CommandBuilder::new(resolved.as_os_str());
    cmd.args(&argv[1..]);
    crate::obs::breadcrumb("pty-direct", &format!("program={}", resolved.display()));
    Some(cmd)
}

/// Apply the shared per-pane cwd + environment (cwd, TERM/COLORTERM, fresh
/// PATH) to a `CommandBuilder` regardless of whether it is a direct spawn or a
/// shell wrapper.
fn apply_pane_env(mut cmd: CommandBuilder, cwd: Option<&str>) -> CommandBuilder {
    let dir = cwd
        .filter(|d| std::path::Path::new(d).is_dir())
        .map(|d| d.to_string())
        .or_else(|| dirs::home_dir().map(|h| h.to_string_lossy().into_owned()));
    if let Some(dir) = dir.as_deref() {
        cmd.cwd(dir);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    // Fresh PATH from the registry: CLIs installed after loomux (or its
    // parent terminal) started must still be findable in new panes.
    if let Some(path) = crate::winpath::fresh_path() {
        cmd.env("PATH", path);
    }
    cmd
}

/// Apply per-pane extra environment on top of the shared pane env (#83). Set
/// LAST so an agent pane's injected `PATH` (gh-shim prefix + fresh PATH) and
/// `LOOMUX_GROUP_DIR` win over the defaults from `apply_pane_env`. Empty for a
/// plain human shell, so those panes are byte-for-byte unchanged.
fn apply_extra_env(mut cmd: CommandBuilder, env: &[(String, String)]) -> CommandBuilder {
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd
}

/// Build the shell-wrapper child command — the pre-#78 path and the universal
/// fallback. The `command` string is run *through* the default shell so PATH
/// shims resolve the same way they do in a normal terminal; a plain interactive
/// shell (no command) instead spawns the requested `shell_kind` (#194 P2) with
/// cwd-reporting (OSC 7) shell integration wired in.
fn build_shell_command(
    command: Option<&str>,
    cwd: Option<&str>,
    shell_kind: ShellKind,
) -> CommandBuilder {
    // A command (agent / custom pane) always runs through the default shell —
    // `shell_kind` only selects the *interactive* Terminal shell.
    if let Some(line) = command.filter(|l| !l.trim().is_empty()) {
        let shell = default_shell();
        let mut cmd = CommandBuilder::new(&shell);
        #[cfg(target_os = "windows")]
        {
            if shell.contains("cmd.exe") {
                cmd.args(["/C", line]);
            } else {
                cmd.args(["-NoLogo", "-Command", line]);
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            cmd.args(["-lc", line]);
        }
        return apply_pane_env(cmd, cwd);
    }

    apply_pane_env(interactive_shell_command(shell_kind), cwd)
}

/// Build the child command for a pane — the direct-CLI executable when `argv`
/// resolves to a native image (issue #78), otherwise the shell wrapper. This is
/// the *decision* only (used by tests); the runtime spawn path lives in
/// [`spawn_pane_child`], which additionally retries the shell if the resolved
/// native exe fails to actually spawn.
#[cfg(test)]
fn build_command(
    command: Option<String>,
    argv: Option<Vec<String>>,
    cwd: Option<String>,
) -> CommandBuilder {
    if let Some(direct) = argv.as_deref().and_then(try_direct_command) {
        return apply_pane_env(direct, cwd.as_deref());
    }
    // Agent/custom panes ignore shell_kind; default to PowerShell here.
    build_shell_command(command.as_deref(), cwd.as_deref(), ShellKind::PowerShell)
}

/// Spawn the pane's child on `slave`, applying the direct-CLI-spawn path with a
/// **complete** fall-through to the shell wrapper (issue #78). Returns the child
/// plus whether the DIRECT path was actually used.
///
/// Every failure mode lands on the exact pre-#78 shell behavior: escape hatch,
/// empty argv, unresolved program, or a `.cmd`/`.ps1` shim (all via
/// `try_direct_command` returning `None`) — AND a program that resolves to a
/// native `.exe`/`.com` but then *fails to spawn* (corrupt/truncated PE, an
/// AV/ACL block, an architecture mismatch). That last case is caught here and
/// retried through the shell, so a bad exe can never leave the agent to die at
/// the #106 bind timeout; it degrades to the wrapper that would have run before.
pub fn spawn_pane_child(
    slave: &(dyn portable_pty::SlavePty + Send),
    command: Option<&str>,
    argv: Option<&[String]>,
    cwd: Option<&str>,
    env: &[(String, String)],
    shell_kind: ShellKind,
) -> Result<(Box<dyn portable_pty::Child + Send + Sync>, bool), String> {
    if let Some(direct) = argv.and_then(try_direct_command) {
        let direct = apply_extra_env(apply_pane_env(direct, cwd), env);
        match slave.spawn_command(direct) {
            Ok(child) => return Ok((child, true)),
            Err(e) => {
                // Resolved native exe, but the spawn itself failed. Breadcrumb
                // and drop to the shell wrapper — the same fallback the
                // resolution/shim cases take — rather than failing the pane.
                crate::obs::breadcrumb("pty-direct-fallback", &format!("spawn-failed err={e}"));
            }
        }
    }
    let shell = apply_extra_env(build_shell_command(command, cwd, shell_kind), env);
    slave
        .spawn_command(shell)
        .map(|c| (c, false))
        .map_err(|e| e.to_string())
}

/// Open a ConPTY and spawn the pane's child on it, returning the pane's id.
///
/// Off-thread (#746 — `crate::blocking::run_blocking`, P1 of
/// `doc/design/performance.md`): `openpty` plus `CreateProcess` is a **process
/// spawn**, which INV-2 refuses on the webview thread outright — and the
/// ConPTY handshake in front of it is not bounded by anything loomux controls.
/// Once per pane and human-gestured, so it is not a hot path; it is the one
/// INV-2 names without qualification.
///
/// `state: State<PtyManager>` becomes `app.try_state` INSIDE the closure, the
/// same move `write_pty` and `change_dir` made in #734: a borrowed `State` is
/// not `'static` and cannot cross into the pool. Tauri injects both, and
/// neither appears in the argument object the frontend sends, so the wire
/// contract is byte-identical.
///
/// **Reentrancy.** Two spawns off-thread are two independent ConPTYs — there is
/// no shared resource between them but the id counter and the map, and both are
/// already concurrency-safe on their own terms: `next_id` is an
/// `AtomicU32::fetch_add`, so ids stay unique however the spawns interleave,
/// and the `ptys` insert is one map mutation under a briefly-held lock (the
/// three worker threads it then starts are spawned per pane and share nothing
/// with another pane's).
///
/// What that gives up, stated plainly: the ids two concurrent spawns receive
/// are no longer necessarily in the order the two calls arrived, because the
/// counter is read after the openpty rather than before. Nothing depends on it.
/// An id is an opaque handle returned to its own caller through its own promise
/// — `src/pty.ts` pairs them that way, orchestration binds an agent to whatever
/// id its own spawn returned, and no code anywhere compares two ids for order.
/// A restore that wants panes in a fixed order gets it from the layout it is
/// replaying, not from the counter.
#[tauri::command]
pub async fn spawn_pty(
    app: AppHandle,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
    command: Option<String>,
    // Structured agent invocation (program + args). When present and its
    // program resolves to a native executable, the pane spawns it directly as
    // the ConPTY child instead of wrapping `command` in a shell (issue #78).
    argv: Option<Vec<String>>,
    // Extra per-pane env, set on top of the shared pane env (#83). Agent panes
    // pass the gh-shim PATH prefix + LOOMUX_GROUP_DIR here to enforce the merge
    // gate; a plain human shell passes nothing and is unchanged.
    env: Option<Vec<(String, String)>>,
    // Which interactive shell a Terminal pane wants: "powershell" | "cmd" |
    // "gitbash" (#194 P2). Only consulted for a plain interactive shell (no
    // `command`); unknown/absent falls back to PowerShell explicitly.
    shell_kind: Option<String>,
) -> Result<u32, String> {
    crate::blocking::run_blocking(move || {
        spawn_pty_blocking(app, cols, rows, cwd, command, argv, env, shell_kind)
    })
    .await
}

/// The body of [`spawn_pty`], run on the blocking pool. Split out so the
/// command itself is a one-line delegate with nothing before its first await
/// (INV-1's delegation half — an `async fn` that works inline is polled on the
/// webview thread and freezes it exactly as a sync one would, #724).
#[allow(clippy::too_many_arguments)]
fn spawn_pty_blocking(
    app: AppHandle,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
    command: Option<String>,
    argv: Option<Vec<String>>,
    env: Option<Vec<(String, String)>>,
    shell_kind: Option<String>,
) -> Result<u32, String> {
    let state = app.try_state::<PtyManager>().ok_or("pty state unavailable")?;
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.max(2),
            cols: cols.max(2),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    // Direct-spawn the agent exe when argv resolves to a native image, with a
    // full retry through the shell wrapper on any failure (issue #78). A plain
    // Terminal pane (no argv/command) spawns the requested shell kind (#194 P2).
    let kind = ShellKind::parse(shell_kind.as_deref());
    // A non-empty wire value we don't recognize silently maps to PowerShell in
    // `parse`; breadcrumb it so the "explicit, not silent" fallback holds. `None`
    // stays silent — it's every agent/custom pane's normal path (rev-78 #4).
    if let Some(raw) = shell_kind.as_deref() {
        let norm = raw.trim().to_ascii_lowercase();
        if !norm.is_empty() && !matches!(norm.as_str(), "powershell" | "cmd" | "gitbash") {
            crate::obs::breadcrumb("shell-kind-fallback", &format!("unknown={raw}->powershell"));
        }
    }
    let (mut child, _direct) = spawn_pane_child(
        &*pair.slave,
        command.as_deref(),
        argv.as_deref(),
        cwd.as_deref(),
        env.as_deref().unwrap_or(&[]),
        kind,
    )?;
    drop(pair.slave);

    // Windows: enroll the child in a kill-on-close Job Object so killing this
    // pane reaps its whole descendant tree (issue #78). Fail-soft — a failure
    // is breadcrumbed and the spawn proceeds with pre-#78 teardown behavior.
    #[cfg(target_os = "windows")]
    let job = match child.process_id() {
        Some(pid) => {
            let job = assign_kill_on_close_job(pid);
            if job.is_none() {
                crate::obs::breadcrumb("pty-job-fail", &format!("pid={pid}"));
            }
            job
        }
        None => {
            crate::obs::breadcrumb("pty-job-fail", "no-pid");
            None
        }
    };

    let killer = child.clone_killer();
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;

    let id = state.next_id.fetch_add(1, Ordering::SeqCst) + 1;
    let output = Arc::new(Mutex::new(OutputBuf::default()));
    // #1607: this pane's own stdin writer thread, spawned beside the reader
    // thread below. ConPTY's own guidance — "each of the communication channels
    // is serviced on a separate thread" — now holds for the input channel as
    // literally as it already did for the output one.
    let writer_jobs = spawn_pane_writer(
        id,
        Arc::downgrade(&state.ptys),
        state.neutral_gate_throttle.clone(),
    );
    state.ptys.lock_safe().insert(
        id,
        PtyHandle {
            master: Arc::new(Mutex::new(pair.master)),
            writer: Arc::new(Mutex::new(writer)),
            killer,
            output: output.clone(),
            user_input_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            #[cfg(target_os = "windows")]
            _job: job,
            input_box_len: Arc::new(std::sync::atomic::AtomicI64::new(0)),
            // Record what actually spawned, not what was requested, so the
            // folder-picker cd is truthful even after a discovery-miss fallback.
            shell_kind: effective_shell_kind(kind),
            writer_jobs,
        },
    );

    crate::obs::breadcrumb("pty-open", &format!("id={id} cols={cols} rows={rows}"));

    // Clone what the waiter thread needs out of the manager, then release the
    // handle: `State<'_, PtyManager>` borrows `app`, and the waiter below MOVES
    // `app` into itself. All three are `Arc`s, so this is three pointer copies.
    let ptys = state.ptys.clone();
    let expected_exits = state.expected_exits.clone();
    let neutral_gate_throttle = state.neutral_gate_throttle.clone();
    drop(state);

    // Reader thread: stream output on a single shared channel keyed by id.
    // The frontend router buffers payloads for panes that haven't attached
    // their handler yet, so no output can be lost at startup. A rolling tail
    // is teed into the ring for orchestration's `get_output`.
    //
    // #712: the reader hands chunks to a per-pane COALESCING pump rather than
    // emitting each one. Every `pty-output` event costs a one-shot script
    // compilation on the GUI thread (see `crate::ptyout`'s module doc for the
    // transport chain), so an event rate set by the child's write pattern and
    // multiplied by the pane count saturated that single thread — the app-wide
    // sluggishness in #712. Only the EVENT rate changes: the bytes, their
    // order, and the ring tee below are exactly as before, and the ring is
    // still updated the instant bytes arrive so orchestration's view of a pane
    // (attention scan, question detection, `get_output`) does not move at all.
    let out_app = app.clone();
    let (out_tx, out_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        crate::ptyout::pty_output_pump(out_rx, |batch| {
            let _ = out_app.emit(
                "pty-output",
                OutputPayload {
                    id,
                    data: B64.encode(&batch),
                },
            );
        });
    });
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    {
                        let mut out = output.lock_safe();
                        out.total += n as u64;
                        out.ring.extend(&buf[..n]);
                        let overflow = out.ring.len().saturating_sub(OUTPUT_RING_CAP);
                        if overflow > 0 {
                            out.ring.drain(..overflow);
                        }
                    }
                    // Send failure means the pump thread is gone (only on
                    // shutdown); stop reading rather than spin on a dead pipe.
                    if out_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
        // Dropping `out_tx` here ends the pump, which flushes any residue held
        // inside the window — a child's last bytes before exit must still land.
    });

    // Waiter thread: reap the child, then tear down and notify. Orchestration
    // learns about agent deaths here (authoritative, even if the frontend
    // never noticed the pane). Its three handles were cloned out above.
    std::thread::spawn(move || {
        let status = child.wait();
        // Snapshot the removed handle's output BEFORE it's dropped (#281): the
        // instant this pty leaves the live map, its ring is gone — before this,
        // a caller asking "why did this die" even a moment later got nothing
        // ("terminal already closed"), which is exactly what made a resumed
        // CLI's silent exit-1 opaque. Reading it off the removed handle itself
        // (not the live map) means it survives the removal.
        let removed = ptys.lock_safe().remove(&id);
        let (tail, total) = match &removed {
            Some(h) => {
                let buf = h.output.lock_safe();
                (crate::orchestration::strip_ansi(&buf.ring.iter().copied().collect::<Vec<u8>>()), buf.total)
            }
            None => (String::new(), 0),
        };
        let expected = expected_exits.lock_safe().remove(&id);
        neutral_gate_throttle.lock_safe().remove(&id); // #496 N1: no leak per pty ever opened
        let exit_code = status.ok().map(|s| s.exit_code());
        crate::obs::breadcrumb(
            "pty-exit",
            &format!("id={id} code={exit_code:?} expected={expected} bytes={total}"),
        );
        if let Some(reg) = app.try_state::<Arc<crate::orchestration::OrchRegistry>>() {
            reg.on_pty_exit(id, exit_code, &tail, total, expected);
        }
        let _ = app.emit("pty-exit", ExitPayload { id, exit_code, expected });
    });

    Ok(id)
}

/// What kind of ConPTY the PTY layer will bind to, so the frontend can tune
/// xterm.js accordingly (`windowsPty` option). portable-pty prefers a
/// sideloaded `conpty.dll` + `OpenConsole.exe` next to the executable over
/// the inbox Windows conhost; the inbox one (Windows 10) repaints the whole
/// screen on every resize, which floods scrollback with duplicate frames.
#[derive(Serialize)]
pub struct PtyBackendInfo {
    /// True when a modern conpty.dll sits next to the executable.
    sideloaded_conpty: bool,
    /// Effective conhost build for xterm's `windowsPty.buildNumber`
    /// (>= 21376 means xterm may keep its own reflow enabled). 0 on
    /// non-Windows platforms, where the option must not be set at all.
    conpty_build: u32,
}

#[tauri::command]
pub fn pty_backend_info() -> PtyBackendInfo {
    #[cfg(target_os = "windows")]
    {
        let sideloaded = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("conpty.dll").is_file()))
            .unwrap_or(false);
        PtyBackendInfo {
            sideloaded_conpty: sideloaded,
            // The sideloaded conhost tracks the Windows Terminal releases
            // (modern resize handling); the inbox Win10 conhost is stuck on
            // the 19041 console codebase regardless of patch level.
            conpty_build: if sideloaded { 22621 } else { 19045 },
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        PtyBackendInfo {
            sideloaded_conpty: false,
            conpty_build: 0,
        }
    }
}

/// #518: `human` says whether this write ORIGINATED in a genuine keyboard or
/// paste event, as opposed to data the terminal manufactured on its own
/// (a query auto-reply). The frontend decides it structurally — see
/// `note_user_input`'s doc and `src/humanorigin.ts` — because byte shape
/// alone cannot, over an open set of reply shapes.
///
/// Optional, and absent means `true`: a caller that says nothing gets exactly
/// the pre-#518 behaviour, and "assume a human typed" is the fail-safe
/// default (it only ever makes delivery hold MORE). The parameter is additive,
/// so the command's existing shape stays valid — `write_pty(id, data)` still
/// compiles, still deserializes, and still means what it always meant.
/// #719: `async`, and the whole body — not just the write — leaves the webview
/// thread. Tauri dispatches a *synchronous* command by calling it directly on
/// the webview main thread, and polls an async command's future there too up to
/// its first real await (the #716/#724 finding), so anything left before that
/// await would still run on the GUI thread. What is left here is a channel
/// send. `note_user_input` in particular moves off it: its throttled
/// `phantom-input-gated` breadcrumb is a file write, which has no business on
/// the thread that paints.
///
/// **#1607 (epic #1600 Phase 2.3): the destination is no longer the shared
/// blocking pool.** #719/#734 handed the body to
/// `tauri::async_runtime::spawn_blocking`, and that pool — tokio's default 512
/// threads, unconfigured anywhere in this tree — is the SAME one every
/// converted `orch_*` command uses. beta6 filled it: polled orchestration
/// commands parked on a registry mutex accumulated there at 2.5-5 per second
/// until `write_pty`'s task could no longer be scheduled, its promise never
/// resolved, and `src/ptywrite.ts`'s per-pane chain stopped dispatching — every
/// pane, at once, refusing input while the window kept painting (#1600 §1.2).
/// The body now goes to a thread that belongs to this pane (`PaneWriter`), so
/// the app's most latency-critical path competes for threads with nothing.
///
/// The alternative shapes were considered and rejected in the plan: a bigger
/// `max_blocking_threads` only moves the cliff, and a dedicated small pool of
/// size k is the same cliff at k wedged panes — sizing k to the pane count IS
/// this, plus bookkeeping.
///
/// **What this does NOT change is ordering, and that is deliberate.** This
/// command still resolves only once the bytes are actually out, because the
/// frontend's ordered writer (`src/ptywrite.ts`, issue #65) keeps exactly one
/// `write_pty` in flight per pane and chains the next one on this promise —
/// that chain, not the backend, is what makes a bracketed paste's terminator
/// unable to overtake its body. Returning early (a queue, a fire-and-forget
/// send) would dissolve both that ordering guarantee and the end-to-end back
/// pressure it rests on: today a pane whose child has stopped reading simply
/// stops accepting chunks, so the unsent remainder waits in the pane's own JS
/// queue and NOTHING accumulates backend-side. A backend queue would have to
/// answer "and what when it fills?" with either unbounded memory or dropped
/// keystrokes — a truncated command line submitted by the Enter behind it.
/// Preserving back pressure is the bounded-memory answer. Only the *thread*
/// and the *lock scope* change — and #1607 changes only *which* thread again,
/// never this. `enqueue_frontend_write` returns a completion channel and this
/// command awaits it, so the promise still resolves exactly when the bytes are
/// out. Nothing runs before that await but a map lookup and a channel send;
/// `PtyManager::enqueue_frontend_write`'s doc carries that argument in full.
#[tauri::command]
pub async fn write_pty(
    app: AppHandle,
    id: u32,
    data: String,
    human: Option<bool>,
) -> Result<(), String> {
    let mut reply = {
        let state = app.try_state::<PtyManager>().ok_or("pty state unavailable")?;
        state.enqueue_frontend_write(id, data, human.unwrap_or(true))?
    };
    // `None` means the writer thread dropped its reply without answering, which
    // it cannot do while it is running — so this is the pane's handle having
    // gone away underneath the job. An error, never a silent `Ok`: the frontend
    // chain must not treat unwritten bytes as delivered.
    reply.recv().await.unwrap_or_else(|| Err("pty writer gone".to_string()))
}

/// Deliberately still synchronous, unlike `write_pty` above (#719).
///
/// The global-lock half of the issue applies here and is fixed: the ConPTY
/// call now happens outside the `ptys` map lock, so a resize can neither wait
/// behind another pane's work nor make another pane wait behind it. Going
/// further and moving it off the main thread would be a bad trade.
///
/// **ASSUMED, not documented (rev finding on #734):** that a resize is bounded
/// local work and cannot park on the child the way `write_all` can. The
/// `ResizePseudoConsole` reference is 162 words and says only "Resizes the
/// internal buffers for a pseudoconsole to the given size" — it is SILENT on
/// blocking, on synchrony, and on the attached application
/// (learn.microsoft.com/en-us/windows/console/resizepseudoconsole). The
/// supporting evidence is observational and this repo's own: #432 was about
/// resize bursts being *expensive*, never about one failing to return, and
/// #719 fingered `write_all` specifically while resize had been running
/// synchronously on this very thread all along. That is evidence, not proof —
/// if a resize-shaped GUI freeze is ever observed, THIS is the assumption to
/// break first, and the fix then is a sequence-guarded async resize (an
/// unguarded one reintroduces the reorder below), not simply `async`.
///
/// The second reason does not depend on the first, which is why the deviation
/// survives the assumption being wrong:
/// a sync command inherits arrival ordering from the main thread's own
/// dispatch, and resizes NEED it: `shouldResizePty` (src/panefit.ts) suppresses
/// only an *identically sized* in-flight call, so two different sizes can be
/// outstanding at once, and off-thread they could land in either order — which
/// would leave ConPTY at the older geometry with no event to correct it. The
/// ordering is worth more here than the few milliseconds.
#[tauri::command]
pub fn resize_pty(state: State<PtyManager>, id: u32, cols: u16, rows: u16) -> Result<(), String> {
    let master = state.master_handle(id).ok_or("pty not found")?;
    let master = master.lock_safe();
    master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| {
            // Resize *failures* are breadcrumbed (a ConPTY resize is one of the
            // heavier operations and interesting near a crash); routine
            // successes are not — a ResizeObserver can fire them in bursts and
            // the spam would bury signal and rotate real crumbs away.
            let e = e.to_string();
            crate::obs::breadcrumb("pty-resize-fail", &format!("id={id} cols={cols} rows={rows} err={e}"));
            e
        })
}

/// Display metadata for a directory the shell just reported via OSC 7.
#[derive(Serialize)]
pub struct DirInfo {
    /// The directory, home-abbreviated to `~` for compact display.
    cwd: String,
    /// Checked-out branch, or a short commit hash when detached; None if the
    /// directory isn't inside a git repository.
    branch: Option<String>,
}

/// Resolve display name + git branch for a shell-reported directory. Called
/// from the frontend each time a pane emits its working directory.
///
/// Off-thread (#746 — `crate::blocking::run_blocking`, P1 of
/// `doc/design/performance.md`): `git_branch` stats and reads `.git` and `HEAD`
/// walking up the parent directories.
///
/// **Why converted rather than left to the throttle.** #743's plan allowed this
/// row to be absorbed by S5's caller-side bound instead, and #764 (S5) has since
/// landed: `refreshthrottle.ts` holds a pane to one repo-signal reaction per
/// `REPO_SIGNAL_WINDOW_MS`, which is what stopped a rebase or an agent's commit
/// loop from driving this at burst rate. That fixed the RATE and not the STALL.
/// A throttle bounds how OFTEN the webview thread pays; it says nothing about
/// what one payment costs, and one payment here is an unbounded walk up a
/// directory chain — on a cold or network path, or a deep tree, a visible freeze
/// that the throttle then guarantees will recur every window. INV-2 admits
/// filesystem work on that thread only as declared debt or as an `exception`
/// with a stated bound, and "≤1 per 500 ms" is a bound on frequency, not on
/// latency. So the row is drained the way the other twenty-four are.
///
/// **Reentrancy.** A pure function of `path`: no state, no lock, nothing
/// written. Two panes reporting the same cwd read the same files and compute
/// the same answer, and a `git checkout` landing mid-read yields the branch on
/// one side of it or the other — which is what a `HEAD` read has always been,
/// since nothing has ever held that repo still.
#[tauri::command]
pub async fn dir_info(path: String) -> DirInfo {
    crate::blocking::run_blocking(move || {
        let dir = Path::new(&path);
        DirInfo {
            cwd: abbreviate_home(dir),
            branch: git_branch(dir),
        }
    })
    .await
}

/// Send a `cd` into a pane's shell, so the folder picker can drive it. The
/// command is formatted for the pane's *own* shell kind (#194 P2), not the
/// machine default — a cmd or Git Bash pane must not receive PowerShell syntax.
///
/// #719: async for the same reason as `write_pty` — this is the same
/// `write_all` into the same pipe, and a pane whose child is not draining
/// wedges it exactly as hard.
///
/// What that gave up, stated plainly: while both were synchronous they were
/// ordered against each other by the main thread's own dispatch (the same
/// accidental mutual exclusion #716 called out), and once both went to a shared
/// pool they were not — a `cd` and a keystroke issued within the same instant
/// could land in either order. Nothing depended on that order. Each write was
/// still atomic against the other (the pane's writer lock), so neither could
/// appear inside the other, and a human is either steering the folder picker or
/// typing into the pane — not both in the same instant.
///
/// **#1607 gives that order back.** Both bodies now go to the same per-pane
/// writer thread, which runs its queue in arrival order, so `cd`-vs-keystroke
/// on one pane is ordered by arrival again — the one property this change adds
/// rather than preserves. What must NOT reorder is a pane's own keystroke
/// stream against itself, and that is still `write_pty`'s frontend chain (#65),
/// which this does not touch.
#[tauri::command]
pub async fn change_dir(app: AppHandle, id: u32, path: String) -> Result<(), String> {
    let mut reply = {
        let state = app.try_state::<PtyManager>().ok_or("pty state unavailable")?;
        state.enqueue_cd(id, path)?
    };
    reply.recv().await.unwrap_or_else(|| Err("pty writer gone".to_string()))
}

/// Build a shell-appropriate `cd` command line (Enter-terminated) for the pane's
/// shell `kind`, tolerating spaces and quotes in `path` (rev-78 #3).
fn cd_command_line(path: &str, kind: ShellKind) -> String {
    #[cfg(target_os = "windows")]
    {
        match kind {
            ShellKind::Cmd => format!("cd /d \"{path}\"\r"),
            // Git Bash: MSYS `cd` accepts a Windows path; POSIX single-quote it
            // (' -> '\'') so spaces/quotes survive.
            ShellKind::GitBash => format!("cd '{}'\r", path.replace('\'', "'\\''")),
            ShellKind::PowerShell => {
                // A PowerShell pane with no pwsh installed degrades to cmd
                // (`powershell_shell_command`), so mirror that here.
                if default_shell().contains("cmd.exe") {
                    format!("cd /d \"{path}\"\r")
                } else {
                    // PowerShell: single-quote and double any embedded quotes.
                    format!("Set-Location -LiteralPath '{}'\r", path.replace('\'', "''"))
                }
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = kind;
        // POSIX single-quote escaping: ' -> '\''
        format!("cd '{}'\r", path.replace('\'', "'\\''"))
    }
}

/// Replace a leading home-directory component with `~` for compact display.
fn abbreviate_home(dir: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = dir.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.to_string_lossy().replace('\\', "/"));
        }
    }
    dir.to_string_lossy().into_owned()
}

/// Resolve the current git branch by walking up from `dir` to the repository
/// root and parsing `.git/HEAD` — no `git` subprocess required. Supports the
/// `.git`-as-a-file form used by worktrees and submodules.
fn git_branch(dir: &Path) -> Option<String> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let dot_git = d.join(".git");
        if let Some(head) = read_head(&dot_git) {
            return parse_head(&head);
        }
        cur = d.parent();
    }
    None
}

/// Load the HEAD contents for a `.git` entry, which may be a directory or a
/// `gitdir: <path>` pointer file.
fn read_head(dot_git: &Path) -> Option<String> {
    let meta = std::fs::metadata(dot_git).ok()?;
    let git_dir = if meta.is_dir() {
        dot_git.to_path_buf()
    } else {
        let pointer = std::fs::read_to_string(dot_git).ok()?;
        let rel = pointer.trim().strip_prefix("gitdir:")?.trim();
        let path = Path::new(rel);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            dot_git.parent()?.join(path)
        }
    };
    std::fs::read_to_string(git_dir.join("HEAD")).ok()
}

/// Branch name from `ref: refs/heads/<name>`, else a short detached-HEAD hash.
fn parse_head(head: &str) -> Option<String> {
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let name = reference.trim().rsplit('/').next()?.trim();
        (!name.is_empty()).then(|| name.to_string())
    } else if head.len() >= 7 {
        Some(head[..7].to_string())
    } else {
        None
    }
}

#[tauri::command]
pub fn kill_pty(state: State<PtyManager>, id: u32) -> Result<(), String> {
    // Remove first so the handle (and its master side) drops; then signal
    // the child. The waiter thread emits pty-exit once it reaps.
    state.kill(id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_head_branch() {
        assert_eq!(parse_head("ref: refs/heads/main\n").as_deref(), Some("main"));
        assert_eq!(
            parse_head("ref: refs/heads/feature/api-v2").as_deref(),
            Some("api-v2")
        );
    }

    #[test]
    fn parse_head_detached() {
        assert_eq!(
            parse_head("a1b2c3d4e5f6\n").as_deref(),
            Some("a1b2c3d")
        );
    }

    /// Program stored as argv[0] of a `CommandBuilder`, for assertions.
    fn prog(cmd: &CommandBuilder) -> String {
        cmd.get_argv()[0].to_string_lossy().into_owned()
    }

    /// File-name component of argv[0] — what "is this a shell?" assertions
    /// must key on. Never the whole `prog()` path: on a temp-dir fixture the
    /// path carries a random directory segment that can itself contain "sh"
    /// (e.g. `.../shq7f2ab/agent`), which made this flake platform-agnostic
    /// (#183) even though the resolved binary was never a shell.
    fn bin_name(cmd: &CommandBuilder) -> String {
        Path::new(&prog(cmd))
            .file_name()
            .expect("argv[0] must have a file-name component")
            .to_string_lossy()
            .into_owned()
    }

    /// Serializes the tests that mutate process-global env vars
    /// (`LOOMUX_NO_DIRECT_SPAWN`, `CARGO_TARGET_DIR`) so they can't race each
    /// other's reads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// The whole direct-vs-shell decision (issue #78), sequenced in one test so
    /// the escape-hatch env mutation can't race sibling cases run in parallel.
    #[test]
    fn direct_spawn_selection() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Prefix deliberately contains "sh": pins that the shell check below
        // must key on the binary's file name, not the whole path — a random
        // tempdir segment containing "sh" is exactly what made this flake
        // (#183), so the fixture now forces that condition every run instead
        // of leaving it to chance.
        let tmp = tempfile::Builder::new().prefix("agentsh_").tempdir().unwrap();
        let exe = tmp.path().join(if cfg!(windows) { "agent.exe" } else { "agent" });
        std::fs::write(&exe, b"x").unwrap();
        let exe_str = exe.to_string_lossy().into_owned();

        // Native executable + structured argv → spawned DIRECTLY: the child is
        // the agent, not a shell, and its flags are passed verbatim as argv.
        let direct = build_command(
            Some("agent --model opus".into()),
            Some(vec![exe_str.clone(), "--model".into(), "opus".into()]),
            None,
        );
        assert_eq!(prog(&direct), exe_str, "argv[0] must be the resolved agent exe");
        let av = direct.get_argv();
        assert_eq!(av[1], "--model");
        assert_eq!(av[2], "opus");
        assert!(
            !bin_name(&direct).contains("pwsh") && !bin_name(&direct).contains("sh"),
            "a direct spawn must not go through a shell, got binary {:?} (full path {:?})",
            bin_name(&direct),
            prog(&direct)
        );

        // No argv → shell wrapper runs the command string (plain/custom panes).
        let wrapped = build_command(Some("claude --x".into()), None, None);
        assert!(
            wrapped.get_argv().iter().any(|a| a == "claude --x"),
            "the command string must be handed to the shell, got {:?}",
            wrapped.get_argv()
        );

        // Escape hatch: LOOMUX_NO_DIRECT_SPAWN forces the wrapper back on even
        // for a resolvable native exe — the one-env-var rollback.
        std::env::set_var("LOOMUX_NO_DIRECT_SPAWN", "1");
        let hatched = build_command(
            Some("agent --model opus".into()),
            Some(vec![exe_str.clone(), "--model".into(), "opus".into()]),
            None,
        );
        std::env::remove_var("LOOMUX_NO_DIRECT_SPAWN");
        assert!(
            hatched.get_argv().iter().any(|a| a == "agent --model opus"),
            "escape hatch must fall back to the shell string, got {:?}",
            hatched.get_argv()
        );

        // A .cmd/.ps1 shim can't be CreateProcess'd directly → shell fallback.
        #[cfg(windows)]
        {
            let shim = tmp.path().join("agent.cmd");
            std::fs::write(&shim, b"@echo off").unwrap();
            let fell_back = build_command(
                Some("shimline --x".into()),
                Some(vec![shim.to_string_lossy().into_owned(), "--x".into()]),
                None,
            );
            assert!(
                fell_back.get_argv().iter().any(|a| a == "shimline --x"),
                "a shim must keep the shell wrapper, got {:?}",
                fell_back.get_argv()
            );
        }
    }

    #[test]
    fn escape_hatch_parsing() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("LOOMUX_NO_DIRECT_SPAWN");
        assert!(!direct_spawn_disabled(), "unset → direct spawn enabled");
        for on in ["1", "true", "TRUE", "yes", "on"] {
            std::env::set_var("LOOMUX_NO_DIRECT_SPAWN", on);
            assert!(direct_spawn_disabled(), "{on:?} must disable direct spawn");
        }
        for off in ["", "0", "false", "False"] {
            std::env::set_var("LOOMUX_NO_DIRECT_SPAWN", off);
            assert!(!direct_spawn_disabled(), "{off:?} must leave direct spawn on");
        }
        std::env::remove_var("LOOMUX_NO_DIRECT_SPAWN");
    }

    #[test]
    fn shell_kind_parse_maps_wire_values_with_powershell_fallback() {
        assert_eq!(ShellKind::parse(Some("cmd")), ShellKind::Cmd);
        assert_eq!(ShellKind::parse(Some("gitbash")), ShellKind::GitBash);
        assert_eq!(ShellKind::parse(Some("powershell")), ShellKind::PowerShell);
        // Case/whitespace tolerant.
        assert_eq!(ShellKind::parse(Some(" CMD ")), ShellKind::Cmd);
        assert_eq!(ShellKind::parse(Some("GitBash")), ShellKind::GitBash);
        // Unknown and absent both fall back to PowerShell — explicit, never a
        // silent wrong shell (#194 P2).
        assert_eq!(ShellKind::parse(Some("fish")), ShellKind::PowerShell);
        assert_eq!(ShellKind::parse(Some("")), ShellKind::PowerShell);
        assert_eq!(ShellKind::parse(None), ShellKind::PowerShell);
    }

    /// Every argv token of a `CommandBuilder`, joined, for substring assertions.
    #[cfg(windows)]
    fn argv_joined(cmd: &CommandBuilder) -> String {
        cmd.get_argv()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[cfg(windows)]
    #[test]
    fn cmd_kind_spawns_cmd_with_osc7_prompt() {
        // No command → interactive shell for the chosen kind. cmd must be cmd.exe
        // held open with /K and an OSC 7 (`]7;`) PROMPT so the dir chip tracks cd.
        let cmd = build_shell_command(None, None, ShellKind::Cmd);
        assert!(prog(&cmd).to_ascii_lowercase().contains("cmd.exe"));
        let av = argv_joined(&cmd);
        assert!(av.contains("/K"), "cmd must stay open with /K, got {av:?}");
        assert!(av.contains("]7;"), "cmd prompt must emit OSC 7, got {av:?}");
    }

    #[cfg(windows)]
    #[test]
    fn git_bash_kind_launches_login_interactive_bash() {
        // The command builder is pure w.r.t. the resolved bash path, so exercise
        // it directly with a fixture path (discovery is machine-dependent).
        let bash = Path::new(r"C:\Program Files\Git\bin\bash.exe");
        let cmd = git_bash_shell_command(bash);
        assert_eq!(prog(&cmd), bash.to_string_lossy());
        let av = argv_joined(&cmd);
        assert!(av.contains("--login"), "git bash must login-source, got {av:?}");
        assert!(av.contains("-i"), "git bash must be interactive, got {av:?}");
        // OSC 7 must run $PWD through cygpath so a Windows-form path reaches the
        // dir chip / git watch, not MSYS `/c/...` (rev-78 #1).
        let prompt = cmd
            .get_env("PROMPT_COMMAND")
            .and_then(|v| v.to_str())
            .unwrap_or_default();
        assert!(prompt.contains("cygpath"), "OSC 7 must Windows-ify $PWD, got {prompt:?}");
        assert!(prompt.contains("]7;"), "must emit an OSC 7 sequence, got {prompt:?}");
    }

    #[cfg(windows)]
    #[test]
    fn wsl_system32_bash_is_not_git_bash() {
        // WSL's launcher lives under %SystemRoot%; it must never be taken for Git
        // Bash — picking it would spawn a Linux distro in the pane (rev-78 #2).
        let sysroot = Path::new(r"C:\Windows");
        assert!(is_system_bash(
            Path::new(r"C:\Windows\System32\bash.exe"),
            Some(sysroot)
        ));
        // Case-insensitive on both the path and the root.
        assert!(is_system_bash(
            Path::new(r"c:\windows\system32\BASH.EXE"),
            Some(sysroot)
        ));
        // A real Git-for-Windows bash is not excluded.
        assert!(!is_system_bash(
            Path::new(r"C:\Program Files\Git\bin\bash.exe"),
            Some(sysroot)
        ));
        // Even with SystemRoot unreadable, a System32 bash is still rejected.
        assert!(is_system_bash(Path::new(r"C:\Windows\System32\bash.exe"), None));
        assert!(!is_system_bash(Path::new(r"C:\Program Files\Git\bin\bash.exe"), None));
        // Separator normalization: a forward-slash PATH entry can't evade it
        // (rev-78 nit 2).
        assert!(is_system_bash(Path::new("C:/Windows/System32/bash.exe"), Some(sysroot)));
        assert!(is_system_bash(Path::new("C:/Windows/System32/bash.exe"), None));
        // Component-boundary match: a sibling like C:\WindowsFoo is NOT excluded
        // by the C:\Windows prefix.
        assert!(!is_system_bash(Path::new(r"C:\WindowsFoo\bin\bash.exe"), Some(sysroot)));
    }

    #[cfg(windows)]
    #[test]
    fn cd_command_line_matches_the_pane_shell_kind() {
        // Each kind gets its own cd syntax (rev-78 #3): a cmd/Git Bash pane must
        // never receive PowerShell's Set-Location.
        let cmd = cd_command_line(r"C:\a b", ShellKind::Cmd);
        assert_eq!(cmd, "cd /d \"C:\\a b\"\r");
        let bash = cd_command_line(r"C:\a b", ShellKind::GitBash);
        assert_eq!(bash, "cd 'C:\\a b'\r");
        // POSIX quote escaping for Git Bash.
        assert_eq!(cd_command_line("it's", ShellKind::GitBash), "cd 'it'\\''s'\r");
    }

    #[cfg(windows)]
    #[test]
    fn git_exe_to_bash_maps_install_layout() {
        // Git for Windows: cmd\git.exe (on PATH) and bin\git.exe both sit two
        // levels under the root; bash is at bin\bash.exe.
        let from_cmd = git_exe_to_bash(Path::new(r"C:\Program Files\Git\cmd\git.exe")).unwrap();
        assert_eq!(from_cmd, PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"));
        let from_bin = git_exe_to_bash(Path::new(r"C:\Program Files\Git\bin\git.exe")).unwrap();
        assert_eq!(from_bin, PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"));
        // A bare name with no parents can't be mapped.
        assert!(git_exe_to_bash(Path::new("git.exe")).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn git_bash_candidates_preserve_precedence_order() {
        // Pure helper over fixed inputs so precedence is asserted exactly, not
        // vacuously (rev-78 #5): Program Files roots first, in the given order,
        // then the per-user LOCALAPPDATA install last.
        let roots = vec![PathBuf::from(r"C:\PF64"), PathBuf::from(r"C:\PF32")];
        let cands = git_bash_candidates_from(&roots, Some(Path::new(r"C:\Local")));
        assert_eq!(
            cands,
            vec![
                PathBuf::from(r"C:\PF64\Git\bin\bash.exe"),
                PathBuf::from(r"C:\PF32\Git\bin\bash.exe"),
                PathBuf::from(r"C:\Local\Programs\Git\bin\bash.exe"),
            ]
        );
        // No LOCALAPPDATA → just the Program Files candidates, order preserved.
        let no_local = git_bash_candidates_from(&roots, None);
        assert_eq!(
            no_local,
            vec![
                PathBuf::from(r"C:\PF64\Git\bin\bash.exe"),
                PathBuf::from(r"C:\PF32\Git\bin\bash.exe"),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn ssh_candidates_follow_system_root_and_cover_the_wow64_view() {
        // Pure helper over a fixed root (#887 S3), so this asserts the layout and
        // the order rather than whatever this machine happens to have installed.
        // A relocated Windows must resolve too — the paths are built from
        // SystemRoot, never from a hardcoded C:\Windows.
        let cands = ssh_candidates_from(Some(Path::new(r"D:\Win")));
        assert_eq!(
            cands,
            vec![
                PathBuf::from(r"D:\Win\System32\OpenSSH\ssh.exe"),
                PathBuf::from(r"D:\Win\Sysnative\OpenSSH\ssh.exe"),
            ]
        );
        // No SystemRoot → no candidates at all. The fallback must not invent a
        // path to stat: "we don't know where Windows is" is not "try C:\Windows".
        assert!(ssh_candidates_from(None).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn command_pane_ignores_shell_kind() {
        // An agent/custom pane carries a command; shell_kind must not change that
        // it runs through the default shell (the wire value only picks the
        // interactive Terminal shell).
        let wrapped = build_shell_command(Some("claude --x"), None, ShellKind::Cmd);
        assert!(
            wrapped.get_argv().iter().any(|a| a == "claude --x"),
            "the command must be handed to the shell verbatim, got {:?}",
            wrapped.get_argv()
        );
    }

    #[test]
    fn git_branch_walks_up_to_repo_root() {
        // The crate lives inside the loomux repo but has no `.git` of its own,
        // so this exercises the parent walk.
        let here = std::env::current_dir().unwrap();
        assert!(git_branch(&here).is_some());
    }

    #[test]
    fn worktree_pane_env_never_injects_cargo_target_dir() {
        // #263: loomux used to point CARGO_TARGET_DIR at a shared
        // `<main-repo-root>/.loomux-target` for every linked-worktree pane
        // (#134). Removed: concurrent cargo runs in stacked worktrees collided
        // on the shared build-script outputs (os error 32 on OpenConsole.exe,
        // exit 101) and the mechanism was cargo-specific product code besides.
        // A pane's env must now carry CARGO_TARGET_DIR only if the operator set
        // one process-wide themselves — loomux must never compute or set it.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Build the same linked-worktree fixture shape the old shared-target
        // resolver keyed off: a `.git` FILE (not dir) whose gitdir/commondir
        // resolve back to a main checkout.
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("myrepo");
        let wt_gitdir = main.join(".git").join("worktrees").join("feat");
        std::fs::create_dir_all(&wt_gitdir).unwrap();
        std::fs::write(wt_gitdir.join("commondir"), "../..\n").unwrap();
        let wt = root.path().join("myrepo-worktrees").join("feat");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_gitdir.display())).unwrap();
        let wt_str = wt.to_string_lossy().into_owned();

        std::env::remove_var("CARGO_TARGET_DIR");
        let cmd = apply_pane_env(CommandBuilder::new("cmd"), Some(wt_str.as_str()));
        assert!(
            cmd.get_env("CARGO_TARGET_DIR").is_none(),
            "loomux must not inject CARGO_TARGET_DIR for a worktree pane, got {:?}",
            cmd.get_env("CARGO_TARGET_DIR")
        );

        // An operator-set CARGO_TARGET_DIR passes through untouched — loomux
        // must never override or clear a deliberate operator choice.
        std::env::set_var("CARGO_TARGET_DIR", r"C:\operator-chosen-target");
        let cmd = apply_pane_env(CommandBuilder::new("cmd"), Some(wt_str.as_str()));
        assert_eq!(
            cmd.get_env("CARGO_TARGET_DIR"),
            Some(std::ffi::OsStr::new(r"C:\operator-chosen-target")),
            "an operator-set CARGO_TARGET_DIR must pass through unmodified"
        );
        std::env::remove_var("CARGO_TARGET_DIR");
    }
}
