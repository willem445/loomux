//! Load a passphrase-protected SSH key into the user's **own** ssh-agent, once,
//! so an SSH pane connects without a prompt the human has to answer per pane
//! (#2368 slice A).
//!
//! ## What this module does and does not hold
//!
//! orrerix holds no credentials (`doc/design/ssh-panes.md`, "No credentials, and
//! what makes that structural"), and this module does not change that rule — it
//! extends it to a passphrase. The passphrase arrives as one command argument,
//! is written to one `ssh-add` process, and is gone: nothing here persists it,
//! `sshprofiles.json`'s schema is untouched, and its encode/decode allowlists
//! still drop a `passphrase` key at both ends. What ends up holding the
//! *decrypted key* is OpenSSH's own agent, exactly as after a hand-run
//! `ssh-add` — which is the point: the key material never enters orrerix at all.
//!
//! **Residual, stated rather than claimed away.** The passphrase reaches this
//! process as a `String` in Tauri's IPC deserialization, and the webview holds
//! it in an `<input>` buffer before that. Neither copy is under this module's
//! control, so neither is zeroed; `zero_secret` below overwrites only the buffer
//! this module owns. That is a real residual and the honest bound on the claim:
//! orrerix does not *store* the passphrase, and does not pretend to have scrubbed
//! every transient copy of it out of two runtimes it does not own.
//!
//! ## Why a hidden ConPTY rather than askpass, stdin or `-t`
//!
//! Every other way of handing `ssh-add` a passphrase depends on which OpenSSH
//! build is installed. On Windows, `readpass.c` invokes askpass **only** when
//! `SSH_ASKPASS` is set, `SSH_ASKPASS_REQUIRE=force` postdates the Windows 10
//! in-box 8.1p1 client, and Win32-OpenSSH #2115 reports `SSH_ASKPASS` being
//! ignored outright on 8.6p1 — so askpass is version roulette on the baseline.
//! `read_passphrase` is called with `RP_ALLOW_STDIN`, but on a **non-tty** stdin
//! that routes back to askpass, so a pipe is not a way in either. `-t` (lifetime)
//! is ignored by the Windows agent (Win32-OpenSSH #1056).
//!
//! Giving `ssh-add` a real console and answering its prompt is what a human does,
//! and it works on 8.1p1 and 8.6p1 alike. That console is the same
//! `native_pty_system().openpty` call `pty::spawn_pty_blocking` makes — no new
//! mechanism, no new dependency — and it is opened **hidden**: it is never
//! registered with `PtyManager`, never streamed to the frontend, and dropped
//! when this function returns.
//!
//! ## Why the passphrase can never reach a pane
//!
//! It is not passed to anything that builds one. `PaneSetupInput`, `SshPlan`,
//! `PaneOptions`, `PersistedPane` and the spawned argv are all unchanged by this
//! slice; the launcher reads the field into a local, calls this command, and
//! drops it. The passphrase is *unrepresentable* on the pane path rather than
//! merely absent from it.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The worst case a human waits, end to end — **20 s**, not [`SSH_ADD_TIMEOUT`].
///
/// `run_ssh_add` sequences probe → start-attempt → probe → drive, so the two
/// paths that can run longest are `AGENT_PROBE_TIMEOUT` + `AGENT_START_TIMEOUT`
/// + `AGENT_PROBE_TIMEOUT` = 20 s to a `NoAgent` refusal, and
/// `AGENT_PROBE_TIMEOUT` + `SSH_ADD_TIMEOUT` = 20 s to a `Timeout`. The launcher
/// awaits the whole sequence behind one "Connecting…", so this — not the 15 s
/// below — is the number a human experiences. Named here so a reader who edits
/// any one of the three constants can see what they are really changing (#2397
/// review premortem).
pub const WORST_CASE_TOTAL: Duration = Duration::from_secs(20);

/// How long the whole `ssh-add` conversation may take before it is abandoned.
///
/// A bound rather than a wait, for the reason `GH_CAPTURE_TIMEOUT` exists: this
/// runs on the blocking pool, which is a bounded shared resource, and an
/// `ssh-add` parked on a prompt nobody will answer would hold a slot forever.
/// Fifteen seconds is far longer than the local key-decrypt it covers (the agent
/// pipe is local; there is no network in this path) and short enough that a
/// human who mistyped gets an answer rather than a spinner.
pub const SSH_ADD_TIMEOUT: Duration = Duration::from_secs(15);

/// How often the driver checks whether the child has exited, between reads.
///
/// Small enough that a finished `ssh-add` is noticed promptly and large enough
/// that the loop is not a spin: this thread is a blocking-pool slot either way,
/// and the only cost of a step is one `try_wait`.
const POLL_STEP: Duration = Duration::from_millis(50);

/// How long an unanswered-looking answer waits before its Enter is re-sent
/// once. Comfortably longer than a local console round trip and far shorter
/// than [`SSH_ADD_TIMEOUT`], so the re-send happens while the run can still
/// succeed rather than as a formality before the deadline.
const ANSWER_REARM: Duration = Duration::from_secs(2);

/// How long the pty pump gets to deliver whatever it still holds after the
/// child has exited. ConPTY renders a screen rather than a stream, so a process
/// that prints one line and exits immediately can have that line dropped.
const POST_EXIT_DRAIN: Duration = Duration::from_millis(300);

/// How long `ssh-add -l` may take to answer "is there an agent".
const AGENT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long `ssh-agent` gets to ask the service manager to start the service
/// before we give up on it and report the agent absent.
const AGENT_START_TIMEOUT: Duration = Duration::from_secs(10);

/// The one-time, **administrator** step that makes the Windows OpenSSH agent
/// available. Carried in the refusal verbatim rather than described, because a
/// user who is told "your agent is off" without the command has to go and find
/// it. The service ships **Disabled** on Windows, so this is not an edge case —
/// it is the default state of a machine that has never used ssh-agent.
pub const WINDOWS_AGENT_HINT: &str =
    "Windows ships the OpenSSH Authentication Agent disabled. In an Administrator PowerShell: \
     Set-Service ssh-agent -StartupType Automatic; Start-Service ssh-agent";

/// The same refusal off Windows, where there is no service to enable and the
/// agent is whatever the user's session started.
pub const UNIX_AGENT_HINT: &str =
    "No ssh-agent is reachable from orrerix. Start one for your session (eval $(ssh-agent)) and \
     relaunch orrerix so it inherits SSH_AUTH_SOCK.";

/// What one `ssh-add` run did. The wire shape of `ssh_add_identity`, and so a
/// **public contract** — `doc/design/ssh-panes.md` carries it beside the schema
/// table for the same reason that one is written down.
///
/// Tagged as `{kind, …}` rather than serde's default so the frontend switches on
/// one stable string; `src/sshagent.ts` mirrors it and `test/sshagent.test.ts`
/// pins one refusal per variant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SshAddOutcome {
    /// `ssh-add` accepted the passphrase; the agent now holds the key.
    Added,
    /// `ssh-add` rejected it. `detail` is ssh-add's own line, scrubbed.
    BadPassphrase { detail: String },
    /// No agent to add the key to. `hint` is the platform's one-time fix.
    NoAgent { hint: String },
    /// The conversation outran its bound — the child is killed.
    Timeout,
    /// Anything else: ssh-add missing, spawn failure, an unrecognised refusal.
    Failed { detail: String },
}

impl SshAddOutcome {
    /// The variant name, and **only** the variant name — this is the entire
    /// content of the one breadcrumb this module writes. `obs::breadcrumb`'s
    /// contract is "a short kind, a few ids/flags — no free text", and here that
    /// is load-bearing rather than stylistic: `detail` can quote a line a foreign
    /// `ssh-add` printed, and the identity path is the human's own filesystem.
    /// Neither belongs in a log file, so neither is reachable from here.
    pub fn variant(&self) -> &'static str {
        match self {
            SshAddOutcome::Added => "added",
            SshAddOutcome::BadPassphrase { .. } => "bad-passphrase",
            SshAddOutcome::NoAgent { .. } => "no-agent",
            SshAddOutcome::Timeout => "timeout",
            SshAddOutcome::Failed { .. } => "failed",
        }
    }
}

/// One chunk of `ssh-add` output, classified against the vendor's own strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshAddEvent {
    /// `Enter passphrase for %s%s: ` — the first ask. Answer it.
    Prompt,
    /// `Bad passphrase, try again for %s%s: ` — the retry ask. ssh-add loops
    /// until it is given an **empty** passphrase, so this is answered with a
    /// bare newline rather than by retrying, and the run ends here.
    BadPassphrase,
    /// `Identity added: %s (%s)`.
    Added,
    /// `Could not open a connection to your authentication agent.`, or the
    /// Windows-only `Error connecting to agent: …` spelling.
    NoAgent,
    /// Anything else — banner text, a key comment, a blank line.
    Other,
}

/// Classify one chunk of `ssh-add` output.
///
/// Substring tests against the strings in OpenSSH's own `ssh-add.c`, not
/// equality: every one of them is a `printf` template with the key path
/// interpolated, and the prompts carry **no trailing newline** (they are asks,
/// not lines), so what this actually sees is a partial buffer. Matching on the
/// invariant prefix is the only reading that survives that.
///
/// Order is significant and is the reason this is one function rather than a
/// chain of `if`s at the call site: the retry ask (`Bad passphrase, try again
/// for …`) must be recognised before the first ask, or a mistyped passphrase
/// would be answered with the same wrong passphrase forever.
pub fn classify_ssh_add_line(line: &str) -> SshAddEvent {
    // The FIRST ask is tested first, and the retry is matched on the vendor's
    // whole template rather than on `Bad passphrase` alone. Both halves of that
    // are load-bearing, and the reason is that the identity PATH is interpolated
    // into these strings by `ssh-add` itself (`Enter passphrase for %s%s: `):
    // a path containing `Bad passphrase` made the first ask classify as the
    // retry, so the driver sent its give-up instead of the passphrase and
    // refused a launch with the key never offered (#2397 review W3). The two
    // templates are disjoint — the retry says `try again for`, not `Enter
    // passphrase for` — so testing the ask first is total, not a tie-break.
    if line.contains("Enter passphrase for") {
        return SshAddEvent::Prompt;
    }
    if line.contains("Bad passphrase, try again for") {
        return SshAddEvent::BadPassphrase;
    }
    if line.contains("Identity added") {
        return SshAddEvent::Added;
    }
    // Two spellings, both real: the portable string, and the one Win32-OpenSSH
    // prints when the named pipe is not there because the service is stopped.
    if line.contains("Could not open a connection to your authentication agent")
        || line.contains("Error connecting to agent")
    {
        return SshAddEvent::NoAgent;
    }
    SshAddEvent::Other
}

/// Where `ssh-add` lives **given** an `ssh` path — pure layout, no filesystem.
///
/// Beside the resolved `ssh`, deliberately: `pty::find_ssh` already decided
/// which OpenSSH the pane will use (PATH first, then the inbox install), and an
/// `ssh-add` found independently on PATH can be a *different* OpenSSH — Git for
/// Windows' MSYS build is the common case, and its agent is not the one the
/// inbox `ssh.exe` talks to. Pairing them by directory means the key is loaded
/// into the agent the client will actually ask.
///
/// The executable suffix is taken from the `ssh` path itself rather than gated
/// on `cfg(windows)`, so a `.exe` seen on any host stays a `.exe`.
pub fn ssh_add_beside(ssh_path: &Path) -> Option<PathBuf> {
    let dir = ssh_path.parent()?;
    let name = match ssh_path.extension().and_then(|e| e.to_str()) {
        Some(ext) if !ext.is_empty() => format!("ssh-add.{ext}"),
        _ => "ssh-add".to_string(),
    };
    Some(dir.join(name))
}

/// Resolve the `ssh-add` to drive: beside the resolved `ssh` when that exists,
/// otherwise the first `ssh-add` on PATH.
///
/// **What this feature is willing to execute, stated because it widened.** Before
/// #2368 an SSH pane ran exactly one binary the human had named — the `ssh`
/// `discover_ssh` resolved. This module adds two more from that same directory:
/// `ssh-add` (here) and `ssh-agent.exe` (`try_start_agent`), neither named by the
/// human and the second on a refusal path they did not ask for. That is accepted
/// on the same footing as the `ssh` itself — someone who can write the directory
/// a user's `ssh` lives in has already replaced the `ssh`, and the beside-rule is
/// what keeps the agent the client's own. It is not a *narrower* surface than
/// before, and the design note says so rather than leaving the widening implicit
/// (#2397 review premortem).
///
/// The impure half of [`ssh_add_beside`] — one `is_file` and a PATH scan. The
/// fallback exists for a layout the beside-rule cannot serve (an `ssh` shim
/// alone in a directory); it is second, not first, because a PATH `ssh-add` is
/// exactly the mismatched-agent hazard the beside-rule avoids.
fn ssh_add_path(ssh_path: &Path) -> Option<PathBuf> {
    if let Some(beside) = ssh_add_beside(ssh_path) {
        if beside.is_file() {
            return Some(beside);
        }
    }
    let exe = ssh_add_beside(Path::new(ssh_path.file_name()?))?;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(&exe))
        .find(|p| p.is_file())
}

/// Whether an agent is reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentProbe {
    /// An agent answered — with or without identities already loaded.
    Present,
    /// Nothing answered. Adding a key would fail with the vendor's own message.
    Absent,
}

/// Classify `ssh-add -l`'s answer.
///
/// Exit **0** = the agent listed identities; **1** = the agent answered and has
/// none; **2** = it could not be contacted at all. So 0 and 1 are both "there is
/// an agent" and only 2 is not — a distinction worth making because "the agent
/// is empty" is the *normal* state of the machine we are about to add a key to,
/// and treating it as absent would refuse every first key on every machine.
///
/// Pure over the parts of a run that decide it, so the mapping is unit-testable
/// without an agent. The stderr fallback covers a build that reports the failure
/// in text without setting 2.
pub fn classify_agent_probe(code: Option<i32>, stderr: &str) -> AgentProbe {
    if matches!(classify_ssh_add_line(stderr), SshAddEvent::NoAgent) {
        return AgentProbe::Absent;
    }
    match code {
        Some(0) | Some(1) => AgentProbe::Present,
        _ => AgentProbe::Absent,
    }
}

/// Ask `ssh-add -l` whether an agent is reachable, under a bound.
fn probe_agent(ssh_add: &Path) -> AgentProbe {
    let mut cmd = std::process::Command::new(ssh_add);
    cmd.arg("-l");
    no_console(&mut cmd);
    match loomux_engine::subproc::capture_raw_with_timeout(cmd, AGENT_PROBE_TIMEOUT) {
        Ok((status, _stdout, stderr)) => classify_agent_probe(status.code(), &stderr),
        // A capture that could not even run is not evidence of an agent.
        Err(_) => AgentProbe::Absent,
    }
}

/// Windows: no console window for a probe the human never asked to see. The
/// same `CREATE_NO_WINDOW` every other `Command` in this crate sets.
fn no_console(cmd: &mut std::process::Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = cmd;
    }
}

/// One attempt to bring the Windows agent up: `ssh-agent.exe` with no arguments
/// asks the Service Control Manager to start the service.
///
/// It is a *request*, not a start: `agent-main.c` calls `OpenSCManagerW` /
/// `OpenServiceW(SERVICE_START)` and `fatal`s when it cannot, which is exactly
/// what happens while the service is **Disabled** — no start type permits it,
/// elevated or not. So this succeeds on a machine whose service is Manual and
/// fails on the default Disabled one, and the refusal below carries the admin
/// step for the second case rather than this function pretending to cover it.
///
/// Off Windows there is no service to start and this does nothing: an agent is
/// the session's business there, which is what `UNIX_AGENT_HINT` says.
#[cfg(target_os = "windows")]
fn try_start_agent(ssh_add: &Path) {
    let Some(dir) = ssh_add.parent() else { return };
    let agent = dir.join("ssh-agent.exe");
    if !agent.is_file() {
        return;
    }
    let mut cmd = std::process::Command::new(agent);
    no_console(&mut cmd);
    let _ = loomux_engine::subproc::capture_raw_with_timeout(cmd, AGENT_START_TIMEOUT);
}

#[cfg(not(target_os = "windows"))]
fn try_start_agent(_ssh_add: &Path) {
    let _ = AGENT_START_TIMEOUT;
}

/// The platform's one-time fix for "no agent".
fn agent_hint() -> String {
    if cfg!(target_os = "windows") {
        WINDOWS_AGENT_HINT.to_string()
    } else {
        UNIX_AGENT_HINT.to_string()
    }
}

/// Remove `secret` from text that is about to cross a boundary.
///
/// A **belt**, not the mechanism: `ssh-add` reads a passphrase with echo off and
/// never prints it back, so on the real path there is nothing here to remove.
/// What this defends against is the path where the program on the other end of
/// the pty is not the real `ssh-add` — a shim, a wrapper, a `.bat` — and echoes
/// what it was given. `detail` is the one field of an outcome that carries
/// foreign text, so it is the one place a leak could ride out.
///
/// An empty secret returns the text unchanged rather than splicing a marker
/// between every character: "there was no passphrase" is not something to
/// redact, and `String::replace` with an empty pattern would otherwise make the
/// detail unreadable.
pub fn scrub_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    text.replace(secret, "***")
}

/// Overwrite a secret buffer in place.
///
/// `write_volatile` rather than a plain loop or `fill(0)`: the buffer is dead
/// after this and an optimiser is entitled to delete a store nothing reads.
/// No `zeroize` crate — that would be a dependency for six lines, and the module
/// doc above states the residual this does not reach (the IPC and webview
/// copies), which is the honest bound on what any crate here could achieve.
fn zero_secret(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a live, uniquely-borrowed, aligned `u8` from a slice we
        // own for the whole call; a volatile write of one byte through it is
        // exactly a `*b = 0` the compiler may not elide.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

/// Send one answer — the line and its Enter in a **single write**.
///
/// Two writes is how the answer gets lost. Measured on CI: the passphrase was
/// echoed back by the console (so the read was live and cooked-mode echo ran)
/// and the lone `\r` that followed it never took effect, on roughly one run in
/// three, leaving `ssh-add` blocked on a read nobody would ever finish. One
/// buffer removes the second trip through the input pipe entirely.
///
/// The result is **returned rather than dropped**. `let _ = write_all(…)` on
/// this path is precisely what made the failure above invisible: a refusal
/// naming the write is a bug report, and a fifteen-second `Timeout` is not.
///
/// The scratch buffer is zeroed on the way out for the same reason
/// [`zero_secret`] exists — it is a second copy of the passphrase, and the only
/// one this function creates.
fn send_answer(writer: &mut Box<dyn std::io::Write + Send>, line: &[u8]) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(line.len() + 1);
    buf.extend_from_slice(line);
    buf.push(b'\r');
    let result = writer.write_all(&buf).and_then(|()| writer.flush());
    zero_secret(&mut buf);
    result
}

/// Drive one `ssh-add` conversation on a hidden pty and report what it did.
///
/// `argv` is spawned **as argv** through `CommandBuilder` — no shell, so the
/// identity path is never parsed by anything, and there is no quoting layer for
/// a path with a space or an `&` in it to get wrong.
///
/// The loop answers at most one prompt with the passphrase and at most one retry
/// ask with an **empty** line, which is `ssh-add`'s own documented give-up: it
/// re-asks until it is given an empty passphrase, so anything else here would be
/// a spin. Everything is inside `timeout`; on expiry the child is killed and the
/// outcome is [`SshAddOutcome::Timeout`] rather than a wait.
///
/// **The bytes decide the conversation; the exit status decides the verdict.**
/// The transcript is what tells the driver *what to answer and when to give up*,
/// and it is the only thing that can. It is not a reliable verdict, for two
/// reasons that only show up on a real ConPTY: the master never reports EOF when
/// the child dies (the read side stays open while this function holds the
/// master), and ConPTY renders a screen rather than a stream, so a process that
/// prints one line and exits immediately can have that line dropped. So the loop
/// polls `try_wait`, drains briefly after the exit, and falls back to
/// `ssh-add`'s own documented exit code when no line decided it.
///
/// The pty is local to this call: opened here, never registered with
/// `PtyManager`, never streamed anywhere, and dropped on return. Nothing about
/// this conversation is visible in a pane.
pub fn drive_ssh_add(argv: &[String], passphrase: &[u8], timeout: Duration) -> SshAddOutcome {
    drive_ssh_add_with_transcript(argv, passphrase, timeout).0
}

/// [`drive_ssh_add`], returning the raw pty transcript beside the outcome.
///
/// A seam, not a second implementation — the same idiom as
/// `capture_raw_with_failing_wait_for_test`. Production takes `.0` and the
/// transcript is dropped; the transcript never reaches a log, a breadcrumb or
/// the wire, because it is the one buffer that can carry an echoed passphrase
/// (see [`scrub_secret`]).
///
/// It is `pub` for one reason: when this conversation fails on a platform no
/// agent may run it on, "it timed out" is not a diagnosis. What the child
/// actually printed — nothing at all, or a prompt spelled differently than the
/// classifier expects — is the difference between a read bug and a match bug,
/// and CI's log is the only place either can be seen.
#[doc(hidden)] // pub for the integration test: the transcript is the diagnostic
pub fn drive_ssh_add_with_transcript(
    argv: &[String],
    passphrase: &[u8],
    timeout: Duration,
) -> (SshAddOutcome, String) {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};

    let Some((program, args)) = argv.split_first() else {
        return (SshAddOutcome::Failed { detail: "no ssh-add command to run".to_string() }, String::new());
    };
    let pair = match native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            return (SshAddOutcome::Failed { detail: format!("could not open a console: {e}") }, String::new())
        }
    };
    let mut builder = CommandBuilder::new(program);
    for a in args {
        builder.arg(a);
    }
    let child = match pair.slave.spawn_command(builder) {
        Ok(c) => c,
        Err(e) => {
            return (SshAddOutcome::Failed { detail: format!("could not run ssh-add: {e}") }, String::new())
        }
    };
    drop(pair.slave);

    let mut child = child;
    let mut killer = child.clone_killer();
    let mut writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            let _ = killer.kill();
            return (
                SshAddOutcome::Failed { detail: format!("could not write to the console: {e}") },
                String::new(),
            );
        }
    };
    let reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            let _ = killer.kill();
            return (
                SshAddOutcome::Failed { detail: format!("could not read the console: {e}") },
                String::new(),
            );
        }
    };

    // The reader must be its own thread: a pty read blocks until the child
    // writes or exits, and the deadline below has to keep running while it does.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        use std::io::Read as _;
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let deadline = Instant::now() + timeout;
    let mut seen = String::new();
    let mut answered = false;
    let mut gave_up = false;
    let mut terminal: Option<SshAddOutcome> = None;
    // `portable_pty`'s own status type, not `std::process`'s: a pty child is
    // reaped through the pty, and the two have similar names and distinct types.
    let mut status: Option<portable_pty::ExitStatus> = None;
    let mut drain_until: Option<Instant> = None;
    // When the last answer was sent, and whether the one permitted re-send has
    // been used. See the re-arm below.
    let mut last_write = Instant::now();
    let mut rearmed = false;
    // How much of `seen` this driver has already acted on. See the classify
    // call below for why the tail, and not the whole transcript, is what is
    // classified.
    let mut consumed = 0usize;

    while terminal.is_none() {
        let now = Instant::now();
        if now >= deadline {
            let _ = killer.kill();
            let _ = child.wait();
            return (SshAddOutcome::Timeout, seen);
        }
        // The child's EXIT is the other way this conversation ends, and it has
        // to be polled for: a ConPTY master read does not return EOF when the
        // child dies, because the read side stays open as long as WE hold the
        // master. Without this the loop has no way to learn the child is gone
        // and every run walks to the deadline — measured on CI, where all three
        // prompting fixtures answered correctly, exited, and were still reported
        // `Timeout` (#2368). `spawn_pty_blocking` does not need it because a
        // pane's death is reported by its own `child.wait()` waiter thread.
        if status.is_none() {
            if let Ok(Some(st)) = child.try_wait() {
                status = Some(st);
                // ConPTY renders a SCREEN, so the last frame before a fast exit
                // can be dropped: the success fixture's `Identity added` line
                // never arrived at all. Give the pump a moment to deliver
                // whatever it still has before deciding.
                drain_until = Some(now + POST_EXIT_DRAIN);
            }
        }
        if drain_until.is_some_and(|until| now >= until) {
            break;
        }
        // One re-send of the answer, and only one.
        //
        // It re-sends **what the reader is still waiting for**, which is not
        // always an empty line. When the answer was two writes, only the
        // trailing `\r` could go missing and the parked reader was always the
        // retry ask — so a bare Enter was the right thing to repeat. Since
        // `send_answer` made it ONE buffer the surviving residual is the
        // opposite shape: the whole write is lost, and the reader still parked
        // is then the FIRST ask, where a bare Enter is an *empty passphrase*
        // and `ssh-add` answers `Bad passphrase, try again for …` — reporting a
        // CORRECT passphrase as rejected (#2397 review W2). So repeat the
        // passphrase while the first ask is what is outstanding, and the empty
        // give-up once it is not.
        //
        // A lost write is indistinguishable from a slow one, and the cost of
        // being wrong stays asymmetric: a duplicate answer is read by nobody if
        // the first one landed, while a lost one costs the user the whole bound
        // and refuses a launch that would have worked.
        if (answered || gave_up)
            && !rearmed
            && status.is_none()
            && now.saturating_duration_since(last_write) >= ANSWER_REARM
        {
            rearmed = true;
            let _ = send_answer(&mut writer, if gave_up { b"" } else { passphrase });
        }
        // Step rather than wait-to-deadline: the exit poll above only runs
        // between receives, so a long block here would defeat it.
        let step = POLL_STEP.min(deadline.saturating_duration_since(now));
        let chunk = match rx.recv_timeout(step) {
            Ok(c) => c,
            // Reachable only once the master is dropped, which is after this
            // function returns — kept as the correctness arm rather than as the
            // mechanism, which is the exit poll above.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
        };
        seen.push_str(&String::from_utf8_lossy(&chunk));

        // Classify the UNCONSUMED TAIL — everything since the last thing this
        // driver acted on — rather than a line or the whole transcript.
        //
        // Not a line: the two asks carry no trailing newline, so a
        // line-oriented read would never see either until the process waiting
        // for an answer had exited. Not the whole transcript either: once the
        // first ask has been answered its text stays in `seen` forever, so
        // every later chunk re-classifies as `Prompt` and nothing after it can
        // ever be recognised (#2397 review W3 / rev-std 1). The tail keeps the
        // no-newline property — it spans every chunk since the last action, so
        // an ask split across reads still matches — while making each ask
        // decidable once.
        //
        // `consumed` is only ever set to `seen.len()`, and `seen` is built from
        // `from_utf8_lossy`, so the slice below is always on a char boundary.
        let tail = &seen[consumed..];
        match classify_ssh_add_line(tail) {
            SshAddEvent::Added => terminal = Some(SshAddOutcome::Added),
            SshAddEvent::NoAgent => terminal = Some(SshAddOutcome::NoAgent { hint: agent_hint() }),
            SshAddEvent::BadPassphrase => {
                if !gave_up {
                    gave_up = true;
                    consumed = seen.len();
                    last_write = Instant::now();
                    // ssh-add's own exit: an empty passphrase ends the retry
                    // loop. Anything else re-asks forever.
                    if let Err(e) = send_answer(&mut writer, b"") {
                        terminal = Some(SshAddOutcome::Failed {
                            detail: format!("could not answer ssh-add: {e}"),
                        });
                    }
                }
            }
            SshAddEvent::Prompt => {
                if !answered {
                    answered = true;
                    consumed = seen.len();
                    last_write = Instant::now();
                }
            }
            SshAddEvent::Other => {}
        }
    }

    let _ = killer.kill();
    let status = match status {
        Some(st) => Some(st),
        None => child.wait().ok(),
    };
    let secret = String::from_utf8_lossy(passphrase).into_owned();
    let detail = scrub_secret(last_meaningful_line(&seen), &secret);
    let outcome = match terminal {
        Some(other) => other,
        None if gave_up => SshAddOutcome::BadPassphrase { detail },
        // No terminal line, but the child exited **0**. `ssh-add` documents
        // that as "the identity was added", and it is a stronger signal than
        // the transcript precisely because the transcript can lose the last
        // frame (see the drain above). The bytes decide the CONVERSATION — what
        // to answer, and when to give up; the exit status decides the VERDICT
        // when the bytes ran out first.
        // `as_ref` because a match guard may not move out of the binding it
        // reads, and this status is not `Copy`.
        None if status.as_ref().is_some_and(|st| st.success()) => SshAddOutcome::Added,
        None => SshAddOutcome::Failed { detail },
    };
    (outcome, seen)
}

/// The last non-blank line of a transcript — what a human reading the console
/// would take as "what it said". Trailing `\r` is stripped because a pty echoes
/// CRLF, and an empty transcript reports itself as such rather than as an empty
/// quote the reader cannot place.
pub fn last_meaningful_line(transcript: &str) -> &str {
    transcript
        .lines()
        .map(|l| l.trim_end_matches('\r').trim())
        .filter(|l| !l.is_empty())
        .next_back()
        .unwrap_or("ssh-add said nothing")
}

/// Load one passphrase-protected identity into the user's ssh-agent (#2368).
///
/// Off-thread (#746 — `crate::blocking::run_blocking`, P1 of
/// `doc/design/performance.md`) and **never sync**: this spawns processes and
/// waits up to [`SSH_ADD_TIMEOUT`] for one. CLAUDE.md constraint 10 refuses a
/// synchronous command that can panic on the webview thread; INV-2 refuses a
/// *process spawn* there outright, and this makes two. The body is a one-line
/// delegate with nothing before its first await, which is INV-1's other half —
/// an `async fn` that works inline is polled on the webview thread and freezes
/// it exactly as a sync one would (#724).
///
/// **Reentrancy.** Two concurrent calls are two independent hidden ptys and two
/// `ssh-add` processes; they share no orrerix state at all (no lock, no map, no
/// counter). They do share the *agent*, which serialises them itself — adding
/// the same key twice is idempotent, and adding two different keys is what an
/// agent is for.
#[tauri::command]
pub async fn ssh_add_identity(
    ssh_path: String,
    identity_file: String,
    passphrase: String,
) -> Result<SshAddOutcome, String> {
    crate::blocking::run_blocking(move || ssh_add_blocking(ssh_path, identity_file, passphrase))
        .await
}

/// The body of [`ssh_add_identity`], run on the blocking pool.
fn ssh_add_blocking(
    ssh_path: String,
    identity_file: String,
    passphrase: String,
) -> Result<SshAddOutcome, String> {
    let mut secret = passphrase.into_bytes();
    let outcome = run_ssh_add(&ssh_path, &identity_file, &secret);
    zero_secret(&mut secret);
    // Exactly one breadcrumb, carrying exactly the variant name: not `detail`
    // (foreign text), not the identity path (the human's filesystem), never the
    // passphrase. See `SshAddOutcome::variant`.
    crate::obs::breadcrumb("ssh-add", &format!("outcome={}", outcome.variant()));
    Ok(outcome)
}

/// Resolve, probe, drive. Split out so the breadcrumb and the zeroing above
/// happen on **every** return path rather than at each of five of them.
fn run_ssh_add(ssh_path: &str, identity_file: &str, passphrase: &[u8]) -> SshAddOutcome {
    let Some(ssh_add) = ssh_add_path(Path::new(ssh_path)) else {
        return SshAddOutcome::Failed {
            detail: "ssh-add was not found beside your ssh client or on PATH.".to_string(),
        };
    };
    if probe_agent(&ssh_add) == AgentProbe::Absent {
        // One attempt to bring it up, then one re-probe. A machine whose agent
        // is merely stopped is the common case; a Disabled service is the one
        // the hint is for.
        try_start_agent(&ssh_add);
        if probe_agent(&ssh_add) == AgentProbe::Absent {
            return SshAddOutcome::NoAgent { hint: agent_hint() };
        }
    }
    let argv = vec![ssh_add.to_string_lossy().into_owned(), identity_file.to_string()];
    drive_ssh_add(&argv, passphrase, SSH_ADD_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- classify_ssh_add_line: the vendor's own strings ----------

    #[test]
    fn the_first_ask_is_a_prompt() {
        // `ssh-add.c`: `Enter passphrase for %s%s: ` — the key path is
        // interpolated and there is no trailing newline, so what the driver
        // actually holds is this partial buffer.
        assert_eq!(
            classify_ssh_add_line(r"Enter passphrase for C:\Users\a\.ssh\id_ed25519: "),
            SshAddEvent::Prompt
        );
    }

    #[test]
    fn the_retry_ask_is_a_bad_passphrase_not_a_prompt() {
        // The retry is an ask too, and reading it as the FIRST ask would answer
        // it with the same wrong passphrase until the timeout.
        assert_eq!(
            classify_ssh_add_line(r"Bad passphrase, try again for C:\Users\a\.ssh\id_ed25519: "),
            SshAddEvent::BadPassphrase
        );
    }

    #[test]
    fn an_identity_path_cannot_impersonate_the_retry_ask() {
        // #2397 review W3. `ssh-add` interpolates the identity PATH into its own
        // prompt template, and that path is whatever the human typed into
        // **Identity file**. When the retry arm matched on `Bad passphrase`
        // alone and was tested first, a path carrying those two words made the
        // FIRST ask classify as the retry: the driver sent its empty give-up
        // instead of the passphrase, and the launch was refused with the key
        // never offered.
        //
        // Unlike an echoed passphrase this needs no foreign shim — the vendor
        // binary builds the string itself.
        assert_eq!(
            classify_ssh_add_line(r"Enter passphrase for C:\keys\Bad passphrase\id_ed25519: "),
            SshAddEvent::Prompt
        );
        // The same hazard the other way round, which the arm ORDER already
        // covered and must keep covering.
        assert_eq!(
            classify_ssh_add_line(r"Enter passphrase for C:\keys\Identity added\id_ed25519: "),
            SshAddEvent::Prompt
        );
        // …and the real retry is still recognised over the same path, which is
        // what stops this being fixed by simply never returning `BadPassphrase`.
        assert_eq!(
            classify_ssh_add_line(r"Bad passphrase, try again for C:\keys\Bad passphrase\id: "),
            SshAddEvent::BadPassphrase
        );
    }

    #[test]
    fn success_is_added() {
        assert_eq!(
            classify_ssh_add_line("Identity added: /home/a/.ssh/id_ed25519 (a@host)"),
            SshAddEvent::Added
        );
    }

    #[test]
    fn added_is_not_a_bad_passphrase() {
        // Negative control for the ordering test above: the success string must
        // not fall into the first arm, which a looser "passphrase" test would.
        assert_ne!(
            classify_ssh_add_line("Identity added: /home/a/.ssh/id_ed25519 (a@host)"),
            SshAddEvent::BadPassphrase
        );
    }

    #[test]
    fn both_no_agent_spellings_are_recognised() {
        // The portable string…
        assert_eq!(
            classify_ssh_add_line("Could not open a connection to your authentication agent."),
            SshAddEvent::NoAgent
        );
        // …and the Win32-OpenSSH one, which is the spelling that actually shows
        // up on the platform where the service is disabled by default. Missing
        // this one would report a stopped Windows agent as `Failed` with a raw
        // vendor line instead of the refusal carrying the admin step.
        assert_eq!(
            classify_ssh_add_line("Error connecting to agent: No such file or directory"),
            SshAddEvent::NoAgent
        );
    }

    #[test]
    fn a_key_comment_is_not_an_event() {
        assert_eq!(classify_ssh_add_line("agent has no identities."), SshAddEvent::Other);
        assert_eq!(classify_ssh_add_line(""), SshAddEvent::Other);
    }

    // ---------- ssh_add_beside: layout only ----------

    #[test]
    fn ssh_add_sits_beside_the_ssh_that_was_resolved() {
        // Separators are NATIVE on both sides. A Windows-spelled literal has no
        // `parent()` on a unix host — `\` is not a separator there — so hardcoding
        // one turns this into a test about backslashes that fails on two of the
        // three CI platforms while saying nothing about the layout rule.
        let dir = Path::new("OpenSSH");
        assert_eq!(ssh_add_beside(&dir.join("ssh.exe")), Some(dir.join("ssh-add.exe")));
        // The suffix comes from the ssh path, not from a cfg: an extensionless
        // ssh yields an extensionless ssh-add.
        assert_eq!(ssh_add_beside(&dir.join("ssh")), Some(dir.join("ssh-add")));
    }

    #[cfg(windows)]
    #[test]
    fn the_inbox_openssh_layout_resolves_as_spelled() {
        // The production spelling, pinned where `Path` actually parses it: this
        // is the directory `find_ssh` resolves on the Windows 10 baseline, and
        // the pair being in ONE directory is what keeps the client and the agent
        // the same OpenSSH.
        assert_eq!(
            ssh_add_beside(Path::new(r"C:\Windows\System32\OpenSSH\ssh.exe")),
            Some(PathBuf::from(r"C:\Windows\System32\OpenSSH\ssh-add.exe"))
        );
    }

    #[test]
    fn a_bare_program_name_has_no_directory_to_sit_beside() {
        // `Path::new("ssh").parent()` is `Some("")`, so this yields a relative
        // `ssh-add` — pinned so the PATH fallback's input is a known shape
        // rather than an accident.
        assert_eq!(ssh_add_beside(Path::new("ssh")), Some(PathBuf::from("ssh-add")));
        assert_eq!(ssh_add_beside(Path::new("ssh.exe")), Some(PathBuf::from("ssh-add.exe")));
    }

    // ---------- the agent probe ----------

    #[test]
    fn an_empty_agent_is_still_an_agent() {
        // Exit 1 = "the agent answered and holds nothing", which is the normal
        // state of the machine we are about to add the first key to. Reading it
        // as absent would refuse every first key ever added.
        assert_eq!(classify_agent_probe(Some(1), ""), AgentProbe::Present);
        assert_eq!(classify_agent_probe(Some(0), "key stuff"), AgentProbe::Present);
    }

    #[test]
    fn exit_two_or_a_connect_error_is_no_agent() {
        assert_eq!(classify_agent_probe(Some(2), ""), AgentProbe::Absent);
        assert_eq!(classify_agent_probe(None, ""), AgentProbe::Absent);
        // Text wins over a code that did not say so — a build that reports the
        // failure without setting 2 is still reporting no agent.
        assert_eq!(
            classify_agent_probe(Some(1), "Error connecting to agent: No such file or directory"),
            AgentProbe::Absent
        );
    }

    // ---------- the scrub, with its positive control ----------

    #[test]
    fn the_scrub_removes_an_echoed_passphrase() {
        let leaked = "Bad passphrase, try again for key: hunter2";
        let scrubbed = scrub_secret(leaked, "hunter2");
        assert!(
            !scrubbed.contains("hunter2"),
            "an echoed passphrase must not ride out on `detail`"
        );
        assert!(scrubbed.contains("Bad passphrase"), "the rest of the line survives");
    }

    #[test]
    fn the_scrub_leaves_a_clean_detail_alone() {
        // Positive control for the test above: an assertion that a string does
        // NOT contain a secret passes just as well when the scrub mangles
        // everything, so pin that a detail with no secret in it is unchanged.
        let clean = "Identity added: /home/a/.ssh/id_ed25519 (a@host)";
        assert_eq!(scrub_secret(clean, "hunter2"), clean);
    }

    #[test]
    fn an_empty_secret_is_not_redacted_between_every_character() {
        // `String::replace` with an empty pattern splices the marker everywhere.
        // "there was no passphrase" is not something to redact.
        let clean = "Identity added: k";
        assert_eq!(scrub_secret(clean, ""), clean);
    }

    // ---------- the breadcrumb carries the variant and nothing else ----------

    #[test]
    fn every_variant_names_itself_and_carries_no_payload() {
        assert_eq!(SshAddOutcome::Added.variant(), "added");
        assert_eq!(SshAddOutcome::Timeout.variant(), "timeout");
        let bad = SshAddOutcome::BadPassphrase { detail: "hunter2 leaked here".into() };
        assert_eq!(bad.variant(), "bad-passphrase");
        // The property the breadcrumb rests on: `variant` is `&'static str`, so
        // a `detail` carrying foreign text cannot reach the log through it.
        assert!(!bad.variant().contains("hunter2"));
        assert_eq!(SshAddOutcome::NoAgent { hint: "x".into() }.variant(), "no-agent");
        assert_eq!(SshAddOutcome::Failed { detail: "x".into() }.variant(), "failed");
    }

    #[test]
    fn the_last_meaningful_line_is_what_a_human_would_read() {
        assert_eq!(last_meaningful_line("banner\r\nIdentity added: k\r\n"), "Identity added: k");
        assert_eq!(last_meaningful_line("   \r\n\r\n"), "ssh-add said nothing");
    }

    #[test]
    fn the_windows_hint_carries_the_whole_admin_step() {
        // The refusal is only useful if a human can act on it without going to
        // find the command; pin both halves of the one-liner.
        assert!(WINDOWS_AGENT_HINT.contains("Set-Service ssh-agent -StartupType Automatic"));
        assert!(WINDOWS_AGENT_HINT.contains("Start-Service ssh-agent"));
    }

    #[test]
    fn the_stated_worst_case_is_what_the_constants_actually_compose_to() {
        // `WORST_CASE_TOTAL` is a claim on a permanent surface — the module doc,
        // the design note, and the sentence a human reads about how long
        // "Connecting…" can sit there. Editing any one of the three constants
        // it composes reddens this rather than silently falsifying all three.
        assert_eq!(
            WORST_CASE_TOTAL,
            AGENT_PROBE_TIMEOUT + AGENT_START_TIMEOUT + AGENT_PROBE_TIMEOUT,
            "the no-agent path: probe, start-attempt, re-probe"
        );
        assert_eq!(
            WORST_CASE_TOTAL,
            AGENT_PROBE_TIMEOUT + SSH_ADD_TIMEOUT,
            "the timeout path: probe, then the conversation"
        );
    }

    #[test]
    fn zeroing_actually_clears_the_buffer() {
        let mut buf = b"hunter2".to_vec();
        zero_secret(&mut buf);
        assert!(buf.iter().all(|b| *b == 0));
    }
}
