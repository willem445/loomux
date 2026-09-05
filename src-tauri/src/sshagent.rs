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

/// The worst case a human waits, end to end. The launcher awaits the whole
/// sequence behind one modal "Connecting…", so this — not [`SSH_ADD_TIMEOUT`] —
/// is the number a human experiences.
///
/// **It is a bound because the code enforces it, not because someone added up
/// the paths.** The first version of this constant was arithmetic over an
/// enumerated path list, and it was wrong: `run_ssh_add`'s agent check does not
/// return on success, it FALLS THROUGH to the conversation, so
/// probe + start + probe + drive was a real fourth-step path nobody had summed —
/// 35 s against a constant claiming 20, with a test that pinned the two
/// compositions someone had listed and therefore *certified* the false number
/// (#2397 review B1; `CLAUDE.md`'s "a guard's green is evidence about its
/// POPULATION" in its most literal form).
///
/// So the shape changed rather than the number. Everything before the
/// conversation now shares ONE deadline ([`AGENT_SETUP_BUDGET`], enforced by
/// [`ensure_agent`]), and the conversation gets its own full
/// [`SSH_ADD_TIMEOUT`] whatever the setup spent. There is no longer a set of
/// paths to enumerate — every path is setup-then-conversation — which is what
/// makes this total true by construction rather than by a list that the next
/// person to add a step will forget to update.
///
/// The third term is [`CHILD_REAP_BUDGET`], and it is here for the same reason
/// one level down: the conversation ends by KILLING a child, and until #2397
/// review B1 it then waited for that child unbounded. “Enforced” has to survive
/// the last statement, not just the loop — so the reap is bounded too, and its
/// budget is part of this sum rather than a step outside it.
///
/// The fourth term is [`SETUP_STEP_OVERRUN`], and it is the same omission one
/// layer out: each setup step is a `capture_raw_with_timeout`, whose own
/// post-kill reap runs AFTER the `timeout` it was handed and is therefore
/// outside [`AGENT_SETUP_BUDGET`]. The shared budget bounds when a step may
/// **start**; it cannot bound what a step spends past its own deadline. So the
/// setup phase really ends at its budget plus an overrun, and this constant said
/// 27 s against a path that could take 33 — three setup steps, three overruns
/// (#2594 item 2).
///
/// One overrun and not one per step, because [`run_setup_steps`] re-reads the
/// shared deadline BETWEEN steps and stops once it has passed: a step is only
/// ever started while time remains, so at most one step can be in flight when
/// the deadline falls. That is what keeps this a construction rather than a
/// count — a fourth setup step tomorrow costs budget, not another 2 s, and
/// `a_fourth_setup_step_costs_no_more_than_the_third` pins it.
pub const WORST_CASE_TOTAL: Duration = Duration::from_secs(29);

/// How far past [`AGENT_SETUP_BUDGET`] the setup phase can run: one bounded
/// capture's own post-kill reap, which happens after the timeout that capture
/// was given.
///
/// Aliased rather than restated, so a change to the engine's reap budget moves
/// this number instead of silently falsifying it.
const SETUP_STEP_OVERRUN: Duration = loomux_engine::subproc::GH_CAPTURE_REAP_TIMEOUT;

/// One deadline shared by **everything before the conversation** — the first
/// probe, the start attempt, and the re-probe together.
///
/// Shared rather than one ceiling each, because per-step ceilings compose: three
/// steps at 5 + 10 + 5 is a 20 s wait before `ssh-add` is even asked, and it is
/// the machine whose agent the start attempt just recovered — the case
/// `try_start_agent` exists for — that pays it. A shared budget makes the setup
/// phase cost at most this much however many steps inside it run, and it cannot
/// borrow from the conversation.
const AGENT_SETUP_BUDGET: Duration = Duration::from_secs(10);

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

/// How long a killed child gets to actually die before it is abandoned.
///
/// **This is what makes [`WORST_CASE_TOTAL`] a bound at all.** `Child::wait` in
/// `portable-pty` 0.9.0 is `WaitForSingleObject(handle, INFINITE)`, and the kill
/// in front of it cannot be trusted to have worked: `WinChildKiller::kill`
/// returns `Err` when `TerminateProcess` **succeeded** (non-zero) and `Ok(())`
/// when it **failed**, so a kill that did not take is indistinguishable from one
/// that did — by construction, not by our discarding the result. A child that
/// survives `TerminateProcess` (a wedged PKCS#11 or smartcard provider, an AV or
/// filesystem filter stalled on the key path — precisely the state the deadline
/// fires in) would then block this thread forever: a blocking-pool slot gone for
/// the life of that process, and a launcher stuck behind "Connecting…" with its
/// submit latch held and no cancel (#2397 review B1).
///
/// Two seconds, matching `GH_CAPTURE_REAP_TIMEOUT` — the same answer
/// `crates/loomux-engine/src/subproc.rs` gives for the same reason, stated there
/// as "a bound whose last act is an unbounded `wait()` is not a bound". The
/// setup phase already got that treatment through `capture_raw_with_timeout`;
/// this is the conversation phase getting it too.
const CHILD_REAP_BUDGET: Duration = Duration::from_millis(2000);

/// How long the pty pump gets to deliver whatever it still holds after the
/// child has exited. ConPTY renders a screen rather than a stream, so a process
/// that prints one line and exits immediately can have that line dropped.
const POST_EXIT_DRAIN: Duration = Duration::from_millis(300);


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
    classify_ssh_add_chunk(line, true)
}

/// [`classify_ssh_add_line`] over a chunk cut out of a longer stream.
///
/// `at_line_start` says whether `chunk`'s first byte begins a line in the stream
/// it came from, and it exists for one arm: `Identity added` (#2594 item 4).
///
/// `ssh-add` prints that verdict at the start of a line and can print it nowhere
/// else, while the driver's chunk is the transcript tail since it last acted —
/// which, right after the passphrase has been sent, begins in the MIDDLE of the
/// prompt line, where the console's echo of what was typed lands. So a bare
/// substring test reads a passphrase containing `Identity added` as ssh-add's
/// own success: a wrong passphrase reported as a loaded key. The real `ssh-add`
/// reads with echo off and prints nothing back, so this is failure-safe against
/// the vendor binary and reachable only through the same foreign shim
/// [`scrub_secret`] defends against (rev-std round 2 f1 on #2397) — anchored
/// here rather than argued away, because the cost of being wrong is a FALSE
/// SUCCESS and the anchor is free.
///
/// The two asks and the no-agent lines are deliberately left unanchored. The
/// asymmetry is the reason: missing an ask costs the whole bound and refuses a
/// launch that would have worked, and ConPTY renders a screen, so an ask can
/// arrive with a repaint in front of it. Missing a no-agent line costs the
/// hint, not correctness. Only `Added` can turn a miss into a success that did
/// not happen.
pub fn classify_ssh_add_chunk(chunk: &str, at_line_start: bool) -> SshAddEvent {
    // The FIRST ask is tested first, and the retry is matched on the vendor's
    // whole template rather than on `Bad passphrase` alone. Both halves of that
    // are load-bearing, and the reason is that the identity PATH is interpolated
    // into these strings by `ssh-add` itself (`Enter passphrase for %s%s: `):
    // a path containing `Bad passphrase` made the first ask classify as the
    // retry, so the driver sent its give-up instead of the passphrase and
    // refused a launch with the key never offered (#2397 review W3). The two
    // templates are disjoint — the retry says `try again for`, not `Enter
    // passphrase for` — so testing the ask first is total, not a tie-break.
    if chunk.contains("Enter passphrase for") {
        return SshAddEvent::Prompt;
    }
    if chunk.contains("Bad passphrase, try again for") {
        return SshAddEvent::BadPassphrase;
    }
    if starts_a_line(chunk, "Identity added", at_line_start) {
        return SshAddEvent::Added;
    }
    // Two spellings, both real: the portable string, and the one Win32-OpenSSH
    // prints when the named pipe is not there because the service is stopped.
    if chunk.contains("Could not open a connection to your authentication agent")
        || chunk.contains("Error connecting to agent")
    {
        return SshAddEvent::NoAgent;
    }
    SshAddEvent::Other
}

/// Whether `needle` occurs in `hay` at the start of a line.
///
/// `hay`'s own first byte counts as a line start only when `at_line_start` says
/// the stream it was cut from put it there — the property a substring test
/// cannot recover once the chunk has been sliced. Both `\n` and `\r` open a
/// line: a pty echoes CRLF, and a ConPTY repaint can emit a bare `\r`.
fn starts_a_line(hay: &str, needle: &str, at_line_start: bool) -> bool {
    let mut from = 0usize;
    while let Some(offset) = hay[from..].find(needle) {
        let at = from + offset;
        let anchored = match at {
            0 => at_line_start,
            _ => matches!(hay.as_bytes()[at - 1], b'\n' | b'\r'),
        };
        if anchored {
            return true;
        }
        // Past the match rather than one byte on: `at` is a char boundary
        // `find` gave us and `needle` is ASCII, so this stays on one.
        from = at + needle.len();
    }
    false
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
fn probe_agent(ssh_add: &Path, budget: Duration) -> AgentProbe {
    let mut cmd = std::process::Command::new(ssh_add);
    cmd.arg("-l");
    no_console(&mut cmd);
    match loomux_engine::subproc::capture_raw_with_timeout(cmd, budget) {
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
fn try_start_agent(ssh_add: &Path, budget: Duration) {
    let Some(dir) = ssh_add.parent() else { return };
    let agent = dir.join("ssh-agent.exe");
    if !agent.is_file() {
        return;
    }
    let mut cmd = std::process::Command::new(agent);
    no_console(&mut cmd);
    let _ = loomux_engine::subproc::capture_raw_with_timeout(cmd, budget);
}

#[cfg(not(target_os = "windows"))]
fn try_start_agent(_ssh_add: &Path, _budget: Duration) {}

/// Make sure an agent is reachable, inside [`AGENT_SETUP_BUDGET`] **total**.
///
/// Returns `None` when there is an agent to add the key to, or `Some` with the
/// refusal when there is not. The whole phase shares one deadline, so a slow or
/// wedged step spends the budget rather than extending it: three steps that each
/// carried their own ceiling is precisely how the 35 s path in #2397 B1 came to
/// exist.
///
/// An exhausted budget fails **closed**: [`run_setup_steps`] starts no further
/// step and nothing below it reports success, so the run ends in the refusal
/// that carries the platform's fix rather than in a launch with no agent behind
/// it.
///
/// **The real steps are not driven by a timed test, and the reason is worth
/// stating.** Proving the budget against `ssh-add` by the clock needs a program
/// that ignores `-l` and then BLOCKS, executed directly (`probe_agent` runs
/// `<ssh-add> -l`, not a shell). The repo's fixture technique is a `.bat`, which
/// `std::process::Command` cannot launch directly on Windows, and constraint 3
/// forbids the real `ssh-add` and `ssh-agent`. What IS pinned is the phase's
/// shape, on fake steps whose overrun is milliseconds rather than seconds
/// (`a_fourth_setup_step_costs_no_more_than_the_third`), plus the arithmetic
/// above it (`the_stated_worst_case_covers_every_path_because_there_is_only_one_shape`).
/// The step list below — which binaries run, in which order — is still left to
/// inspection. Stated rather than left for a reader to discover the test they
/// expected is absent.
fn ensure_agent(ssh_add: &Path) -> Option<SshAddOutcome> {
    let deadline = Instant::now() + AGENT_SETUP_BUDGET;
    // One attempt to bring it up between the two probes. A machine whose agent
    // is merely stopped is the common case; a Disabled service is the one the
    // hint is for.
    let mut steps: Vec<Box<dyn FnMut(Duration) -> bool + '_>> = vec![
        Box::new(|left| probe_agent(ssh_add, left) == AgentProbe::Present),
        Box::new(|left| {
            try_start_agent(ssh_add, left);
            false
        }),
        Box::new(|left| probe_agent(ssh_add, left) == AgentProbe::Present),
    ];
    if run_setup_steps(deadline, &mut steps) {
        return None;
    }
    Some(SshAddOutcome::NoAgent { hint: agent_hint() })
}

/// Run `steps` under ONE shared deadline, stopping at the first step that
/// settles the question — or at the first moment the deadline has passed.
///
/// Each step is handed the time it has left and answers whether it settled the
/// question. The deadline is re-read BETWEEN steps, so a step is only ever
/// STARTED while time remains, and that ordering is the whole point (#2594
/// item 2). A step costs its own timeout plus at most [`SETUP_STEP_OVERRUN`] —
/// the post-kill reap `capture_raw_with_timeout` runs after the timeout it was
/// handed, which no caller's budget can reach inside. A phase that never starts
/// a step past its deadline therefore ends at that deadline plus ONE overrun,
/// however many steps it has. Checking the clock only INSIDE each step costs
/// one overrun each instead, which is the arithmetic that had
/// [`WORST_CASE_TOTAL`] claiming 27 s against a code path that could take 33.
///
/// Taking the steps as a list rather than inlining them into [`ensure_agent`] is
/// what lets that property be pinned at all: a test can hand this fake steps
/// whose overrun is milliseconds, and add a fourth one to show the bound does
/// not move (`a_fourth_setup_step_costs_no_more_than_the_third`). Inlined, the
/// only pin available would be arithmetic over a step count — which is the
/// enumeration shape #2397 review B1 removed from this module in the first
/// place.
fn run_setup_steps(deadline: Instant, steps: &mut [Box<dyn FnMut(Duration) -> bool + '_>]) -> bool {
    for step in steps.iter_mut() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        if step(left) {
            return true;
        }
    }
    false
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

/// Kill a child and wait for it to die, **bounded**.
///
/// Returns its status when it really did exit inside [`CHILD_REAP_BUDGET`], and
/// `None` when it did not — in which case the child is abandoned rather than
/// waited on, exactly the trade `subproc::abandon_child_and_readers` makes. A
/// leaked process is worse than a reaped one and far better than a wedged app:
/// the alternative here is not "wait a little longer", it is
/// `WaitForSingleObject(INFINITE)` on the webview's blocking pool.
///
/// Polls rather than waits for the same reason `wait_bounded` does — there is no
/// timed wait on this `Child` trait — reusing [`POLL_STEP`], which is already
/// this module's answer to "how often is often enough".
fn reap_bounded(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    killer: &mut Box<dyn portable_pty::ChildKiller + Send + Sync>,
) -> Option<portable_pty::ExitStatus> {
    // The result is deliberately unread: in 0.9.0 it is inverted (see
    // CHILD_REAP_BUDGET), so it carries no information either way. What bounds
    // this is the deadline below, never the kill.
    let _ = killer.kill();
    let deadline = Instant::now() + CHILD_REAP_BUDGET;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL_STEP);
    }
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
    let Some((program, args)) = argv.split_first() else {
        return (SshAddOutcome::Failed { detail: "no ssh-add command to run".to_string() }, String::new());
    };
    let Wired { master, mut child, mut killer, mut writer, rx } = match open_and_wire(program, args)
    {
        Ok(wired) => wired,
        Err(detail) => return (SshAddOutcome::Failed { detail }, String::new()),
    };

    let mut end = run_conversation(&mut child, &mut writer, &rx, passphrase, timeout);

    // ONE bounded reap, on every way out of the conversation.
    //
    // Until #2594 this was two arms and only one of them waited: a run whose
    // outcome a transcript line had already decided merely KILLED the child, on
    // the argument that waiting for a status nobody reads was an unbounded
    // block. Unbounded it is not — the wait here is `reap_bounded`, the same
    // `CHILD_REAP_BUDGET` poll `WORST_CASE_TOTAL` already carries — and off
    // Windows the kill-only arm leaked a zombie per launch: `spawn_command`
    // yields a `std::process::Child` whose `Drop` does no wait, and nothing else
    // in this process ever reaps it. A `Timeout` was cleaned up and a SUCCESS
    // was not, and success is the arm every working launch takes (#2594 item 1).
    if end.status.is_none() {
        end.status = reap_bounded(&mut child, &mut killer);
    }
    let (outcome, seen) = verdict(end, passphrase);
    close_console_while_the_reader_drains(writer, master, rx);
    (outcome, seen)
}

/// The live ends of one hidden console.
struct Wired {
    /// Dropping this is `ClosePseudoConsole` — see
    /// [`close_console_while_the_reader_drains`], which is the only place it
    /// should happen.
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
}

/// Open the hidden console, spawn `program` on its slave end, and start the
/// reader thread draining its master. `Err` carries the `detail` of a
/// [`SshAddOutcome::Failed`].
///
/// Split out of [`drive_ssh_add_with_transcript`] (#2594 item 6), and not only
/// for length: four fallible set-up steps each with their own refusal string
/// were four early returns out of the middle of that function, and an early
/// return is a return that skips the reap and the ordered close its tail now
/// performs exactly once.
fn open_and_wire(program: &str, args: &[String]) -> Result<Wired, String> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};

    let pair = native_pty_system()
        .openpty(PtySize { rows: 24, cols: 120, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| format!("could not open a console: {e}"))?;
    let mut builder = CommandBuilder::new(program);
    for a in args {
        builder.arg(a);
    }
    let mut child =
        pair.slave.spawn_command(builder).map_err(|e| format!("could not run ssh-add: {e}"))?;
    drop(pair.slave);
    let mut killer = child.clone_killer();

    // Past this point the child is LIVE, so a failure here kills and reaps it
    // rather than only killing it — the same zombie the conversation's own tail
    // used to leave off Windows (#2594 item 1), on a path that is rarer but no
    // less real.
    //
    // These two arms return with `pair` still owning the master, so its drop is
    // the `ClosePseudoConsole` that [`close_console_while_the_reader_drains`]
    // exists to order — and there is no reader thread yet, since `spawn_reader`
    // runs only on the `Ok` path. That is deliberate rather than an oversight
    // the ordering rule escaped: the reap above has already ENDED the child, so
    // there is no attached client for the close to wait on, which is the whole
    // condition that made the ordering matter. The reap is therefore
    // load-bearing here for a second reason beyond the zombie, and must stay
    // above both returns (#2661 review, rev-final premortem 2).
    let writer = match pair.master.take_writer() {
        Ok(writer) => writer,
        Err(e) => {
            reap_bounded(&mut child, &mut killer);
            return Err(format!("could not write to the console: {e}"));
        }
    };
    let reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(e) => {
            reap_bounded(&mut child, &mut killer);
            return Err(format!("could not read the console: {e}"));
        }
    };
    Ok(Wired { master: pair.master, child, killer, writer, rx: spawn_reader(reader) })
}

/// Drain the console on its own thread, into a channel the driver can poll.
///
/// It must be a thread: a pty read blocks until the child writes or exits, and
/// the conversation's deadline has to keep running while it does.
fn spawn_reader(reader: Box<dyn std::io::Read + Send>) -> std::sync::mpsc::Receiver<Vec<u8>> {
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
    rx
}

/// Close the console in the one order that cannot block: the input side first,
/// then the console itself **while the reader thread is still draining it**, and
/// the receiving end last.
///
/// The three ends are taken by value in that order, so the ordering is a
/// property of this signature rather than of where three bindings happen to sit
/// in the caller — where it was the reverse, and silently (#2594 item 5).
///
/// Dropping the master is `ClosePseudoConsole`, which is documented to wait for
/// an attached client to finish with the console. Nothing is draining it once
/// `rx` has gone: the reader thread's next `send` fails and it breaks out of its
/// loop, so a close that waits would be waiting on a pipe with no reader — an
/// unbounded block on the blocking pool, which is the one failure every bound in
/// this module exists to prevent. Holding `rx` across the close keeps the thread
/// reading until the console is actually gone, at which point its `read` returns
/// and it exits on its own.
///
/// The thread is not joined, deliberately: a join here would reintroduce the
/// unbounded wait in the very place this function removes it.
fn close_console_while_the_reader_drains(
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
) {
    drop(writer);
    drop(master);
    drop(rx);
}

/// What one conversation ended up having seen and done.
struct ConversationEnd {
    /// Set when a transcript line decided the outcome by itself.
    terminal: Option<SshAddOutcome>,
    /// The child's exit status, once it has been observed — by the loop's own
    /// `try_wait`, or by the caller's bounded reap after it.
    status: Option<portable_pty::ExitStatus>,
    /// Whether the empty-line give-up was sent, i.e. `ssh-add` re-asked.
    gave_up: bool,
    /// The raw pty transcript.
    seen: String,
    /// Whether the loop ended on its deadline rather than on an answer.
    timed_out: bool,
}

/// The driver's state across one conversation: the transcript, how much of it
/// has been acted on, and which of the two one-shot answers have been sent.
struct Conversation {
    seen: String,
    /// How much of `seen` this driver has already acted on. See
    /// [`Conversation::step`] for why the tail, and not the whole transcript,
    /// is what gets classified.
    consumed: usize,
    answered: bool,
    gave_up: bool,
    /// When the last answer was sent, and whether the one permitted re-send has
    /// been used. See [`Conversation::rearm`].
    last_write: Instant,
    rearmed: bool,
}

impl Conversation {
    fn new() -> Self {
        Conversation {
            seen: String::new(),
            consumed: 0,
            answered: false,
            gave_up: false,
            last_write: Instant::now(),
            rearmed: false,
        }
    }

    /// Classify the UNCONSUMED TAIL — everything since the last thing this
    /// driver acted on — and act on it. Returns a terminal outcome when one has
    /// been decided.
    ///
    /// Not a line: the two asks carry no trailing newline, so a line-oriented
    /// read would never see either until the process waiting for an answer had
    /// exited. Not the whole transcript either: once the first ask has been
    /// answered its text stays in `seen` forever, so every later chunk
    /// re-classifies as `Prompt` and nothing after it can ever be recognised
    /// (#2397 review W3 / rev-std 1). The tail keeps the no-newline property —
    /// it spans every chunk since the last action, so an ask split across reads
    /// still matches — while making each ask decidable once.
    ///
    /// `consumed` is only ever set to `seen.len()`, and `seen` is built from
    /// `from_utf8_lossy`, so the slice is always on a char boundary. What it is
    /// NOT always on is a LINE start: right after an answer the tail begins
    /// mid-prompt, exactly where a console echo of what was typed lands. That is
    /// the distinction [`classify_ssh_add_chunk`]'s second argument carries, and
    /// why it is computed here rather than assumed (#2594 item 4).
    fn step(
        &mut self,
        writer: &mut Box<dyn std::io::Write + Send>,
        passphrase: &[u8],
    ) -> Option<SshAddOutcome> {
        let at_line_start =
            self.consumed == 0 || matches!(self.seen.as_bytes()[self.consumed - 1], b'\n' | b'\r');
        // Bound to a local first: a temporary in a `match` scrutinee lives to
        // the end of the `match`, and the arms below take `&mut self`.
        let event = classify_ssh_add_chunk(&self.seen[self.consumed..], at_line_start);
        match event {
            SshAddEvent::Added => Some(SshAddOutcome::Added),
            SshAddEvent::NoAgent => Some(SshAddOutcome::NoAgent { hint: agent_hint() }),
            // ssh-add's own exit: an empty passphrase ends the retry loop.
            // Anything else re-asks forever.
            SshAddEvent::BadPassphrase if !self.gave_up => {
                self.gave_up = true;
                self.answer(writer, b"")
            }
            SshAddEvent::Prompt if !self.answered => {
                self.answered = true;
                self.answer(writer, passphrase)
            }
            _ => None,
        }
    }

    /// Send one answer, mark the tail consumed, and start the re-arm clock.
    fn answer(
        &mut self,
        writer: &mut Box<dyn std::io::Write + Send>,
        line: &[u8],
    ) -> Option<SshAddOutcome> {
        self.consumed = self.seen.len();
        self.last_write = Instant::now();
        write_refusal(send_answer(writer, line))
    }

    /// Whether the one permitted re-send is due.
    fn rearm_due(&self, now: Instant, child_running: bool) -> bool {
        (self.answered || self.gave_up)
            && !self.rearmed
            && child_running
            && now.saturating_duration_since(self.last_write) >= ANSWER_REARM
    }

    /// One re-send of the answer, and only one.
    ///
    /// It re-sends **what the reader is still waiting for**, which is not always
    /// an empty line. When the answer was two writes, only the trailing `\r`
    /// could go missing and the parked reader was always the retry ask — so a
    /// bare Enter was the right thing to repeat. Since [`send_answer`] made it
    /// ONE buffer the surviving residual is the opposite shape: the whole write
    /// is lost, and the reader still parked is then the FIRST ask, where a bare
    /// Enter is an *empty passphrase* and `ssh-add` answers `Bad passphrase, try
    /// again for …` — reporting a CORRECT passphrase as rejected (#2397 review
    /// W2). So repeat the passphrase while the first ask is what is outstanding,
    /// and the empty give-up once it is not.
    ///
    /// A lost write is indistinguishable from a slow one, and the cost of being
    /// wrong stays asymmetric: a duplicate answer is read by nobody if the first
    /// one landed, while a lost one costs the user the whole bound and refuses a
    /// launch that would have worked.
    ///
    /// `consumed` is deliberately NOT advanced: this repeats what is already
    /// outstanding, and swallowing the bytes that arrived meanwhile would lose
    /// the ask or the verdict among them.
    fn rearm(
        &mut self,
        writer: &mut Box<dyn std::io::Write + Send>,
        passphrase: &[u8],
    ) -> Option<SshAddOutcome> {
        // [scratch #2594 R5] the pre-fix dropped result.
        self.rearmed = true;
        let _ = send_answer(writer, if self.gave_up { b"" } else { passphrase });
        None
    }
}

/// Turn a failed write to the console into the refusal it deserves.
///
/// Every answer goes through here, the re-send included. `let _ = send_answer(…)`
/// on the re-arm was the last write on this path whose failure was dropped, and
/// [`send_answer`]'s own argument applies to it unchanged: a refusal naming the
/// write is a bug report, and a fifteen-second `Timeout` is not (#2594 item 3).
/// A re-send that fails is if anything the stronger signal — the first write to
/// the same console already succeeded, so what it reports is a console that has
/// gone away mid-conversation, which no amount of further waiting recovers.
fn write_refusal(result: std::io::Result<()>) -> Option<SshAddOutcome> {
    result
        .err()
        .map(|e| SshAddOutcome::Failed { detail: format!("could not answer ssh-add: {e}") })
}

/// Answer what the child asks until a line decides the outcome, the child
/// exits, or the deadline falls.
///
/// Split out of [`drive_ssh_add_with_transcript`] (#2594 item 6). It reaps
/// nothing and closes nothing — its caller does both, once, on every path —
/// which is why the deadline arm here BREAKS with `timed_out` set instead of
/// returning `Timeout` from the middle of the loop the way it used to.
fn run_conversation(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    writer: &mut Box<dyn std::io::Write + Send>,
    rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    passphrase: &[u8],
    timeout: Duration,
) -> ConversationEnd {
    let deadline = Instant::now() + timeout;
    let mut conv = Conversation::new();
    let mut terminal: Option<SshAddOutcome> = None;
    // `portable_pty`'s own status type, not `std::process`'s: a pty child is
    // reaped through the pty, and the two have similar names and distinct types.
    let mut status: Option<portable_pty::ExitStatus> = None;
    let mut drain_until: Option<Instant> = None;
    let mut timed_out = false;

    while terminal.is_none() {
        let now = Instant::now();
        if now >= deadline {
            timed_out = true;
            break;
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
            if let Ok(Some(exited)) = child.try_wait() {
                status = Some(exited);
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
        if conv.rearm_due(now, status.is_none()) {
            terminal = conv.rearm(writer, passphrase);
            if terminal.is_some() {
                break;
            }
        }
        // Step rather than wait-to-deadline: the exit poll above only runs
        // between receives, so a long block here would defeat it.
        let step = POLL_STEP.min(deadline.saturating_duration_since(now));
        let chunk = match rx.recv_timeout(step) {
            Ok(chunk) => chunk,
            // Reachable only once the master is dropped, which is after the
            // caller has returned — kept as the correctness arm rather than as
            // the mechanism, which is the exit poll above.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
        };
        conv.seen.push_str(&String::from_utf8_lossy(&chunk));
        terminal = conv.step(writer, passphrase);
    }

    ConversationEnd { terminal, status, gave_up: conv.gave_up, seen: conv.seen, timed_out }
}

/// What the run MEANT, given what it saw and what the child's exit said.
///
/// Split out of [`drive_ssh_add_with_transcript`] (#2594 item 6): "how the
/// conversation was driven" and "what it amounts to" are different questions,
/// and separating them is what lets the deadline be one arm here rather than a
/// return from inside a loop.
///
/// Takes the end by value and hands the transcript back, so the one buffer that
/// can carry an echoed passphrase is moved rather than copied.
fn verdict(end: ConversationEnd, passphrase: &[u8]) -> (SshAddOutcome, String) {
    if let Some(decided) = end.terminal {
        return (decided, end.seen);
    }
    if end.timed_out {
        return (SshAddOutcome::Timeout, end.seen);
    }
    let secret = String::from_utf8_lossy(passphrase).into_owned();
    let detail = scrub_secret(last_meaningful_line(&end.seen), &secret);
    if end.gave_up {
        return (SshAddOutcome::BadPassphrase { detail }, end.seen);
    }
    // No terminal line, but the child exited **0**. `ssh-add` documents that as
    // "the identity was added", and it is a stronger signal than the transcript
    // precisely because the transcript can lose the last frame (see the drain in
    // `run_conversation`). The bytes decide the CONVERSATION — what to answer,
    // and when to give up; the exit status decides the VERDICT when the bytes
    // ran out first.
    if end.status.is_some_and(|status| status.success()) {
        return (SshAddOutcome::Added, end.seen);
    }
    (SshAddOutcome::Failed { detail }, end.seen)
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
    // Setup, then the conversation — and only ever those two, which is what
    // makes WORST_CASE_TOTAL a construction rather than a sum over a path list.
    if let Some(refusal) = ensure_agent(&ssh_add) {
        return refusal;
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
    fn an_echoed_passphrase_cannot_impersonate_the_success_line() {
        // #2594 item 4, the echo half of the substring hazard. The driver
        // classifies the transcript tail SINCE its last action, and right after
        // it answers the first ask that tail begins in the middle of the prompt
        // line — which is where a console echo of what was typed lands. A bare
        // substring test therefore reads a passphrase containing the vendor's
        // success wording as the vendor SAYING it: a wrong passphrase reported
        // as a loaded key, which is the one direction of this bug that fails
        // open.
        //
        // `ssh-add` prints `Identity added` at the start of a line and can print
        // it nowhere else, so the anchor costs nothing and closes it.
        assert_eq!(
            classify_ssh_add_chunk("Identity added: /k/id (a@host)", false),
            SshAddEvent::Other,
            "mid-line text is an echo, not ssh-add's verdict"
        );
        // The discriminator: the SAME bytes at a line start are the verdict, so
        // this cannot be passed by never returning `Added` at all.
        assert_eq!(
            classify_ssh_add_chunk("Identity added: /k/id (a@host)", true),
            SshAddEvent::Added
        );
        // …and the shape the driver really sees: the prompt, then the echo, then
        // ssh-add's own line. The tail does not start a line, and the verdict is
        // still found — on the `\r\n` a pty echoes.
        assert_eq!(
            classify_ssh_add_chunk("Identity added\r\nIdentity added: /k/id (a@host)\r\n", false),
            SshAddEvent::Added
        );
    }

    #[test]
    fn a_line_start_is_either_end_of_a_crlf() {
        // A pty echoes CRLF and a ConPTY repaint can emit a bare `\r`, so both
        // open a line. Pinned because the anchor above is only as good as what
        // it counts as a line break.
        assert!(starts_a_line("x\nIdentity added: k", "Identity added", false));
        assert!(starts_a_line("x\rIdentity added: k", "Identity added", false));
        assert!(!starts_a_line("x Identity added: k", "Identity added", false));
        // A first match that is NOT anchored must not hide a later one that is —
        // the loop keeps looking rather than answering on the first hit.
        assert!(starts_a_line("echo Identity added\nIdentity added: k", "Identity added", false));
        // Non-ASCII before the match, so the scan's byte-index arithmetic is
        // exercised on a string where a char boundary is not a byte boundary.
        assert!(starts_a_line("é\nIdentity added: k", "Identity added", false));
        assert!(!starts_a_line("é Identity added: k", "Identity added", false));
    }

    #[test]
    fn the_asks_are_deliberately_not_anchored() {
        // The asymmetry, pinned so a later tidy-up does not "finish the job" and
        // anchor these too. Missing an ask costs the whole bound and refuses a
        // launch that would have worked; missing a no-agent line costs the hint.
        // Only `Added` can turn a miss into a success that did not happen — and
        // ConPTY renders a screen, so an ask can arrive with a repaint in front
        // of it.
        assert_eq!(
            classify_ssh_add_chunk("...Enter passphrase for /k/id: ", false),
            SshAddEvent::Prompt
        );
        assert_eq!(
            classify_ssh_add_chunk("...Bad passphrase, try again for /k/id: ", false),
            SshAddEvent::BadPassphrase
        );
        assert_eq!(
            classify_ssh_add_chunk("...Error connecting to agent: nope", false),
            SshAddEvent::NoAgent
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
    fn the_stated_worst_case_covers_every_path_because_there_is_only_one_shape() {
        // The previous version of this test enumerated two compositions and
        // pinned the constant against them — and `run_ssh_add` had a THIRD,
        // because its agent check falls through to the conversation rather than
        // returning. The constant had been chosen to satisfy the list, so the
        // test certified a false number instead of exposing it (#2397 B1).
        //
        // It is no longer an enumeration. Every path is setup-then-conversation:
        // the setup phase shares one deadline whatever runs inside it, and the
        // conversation always gets its own. So there is one sum, and a step
        // added to `ensure_agent` tomorrow cannot invent a path this misses.
        //
        // The fourth term arrived with #2594 item 2 and is the same omission one
        // layer out: a setup step is a bounded capture, and a bounded capture's
        // own post-kill reap runs AFTER the timeout it was handed, where no
        // caller's budget can reach it. One overrun and not one per step,
        // because `run_setup_steps` starts no step past the shared deadline —
        // which `a_fourth_setup_step_costs_no_more_than_the_third` is what pins,
        // rather than a count that a fourth step would silently falsify.
        assert_eq!(
            WORST_CASE_TOTAL,
            AGENT_SETUP_BUDGET + SETUP_STEP_OVERRUN + SSH_ADD_TIMEOUT + CHILD_REAP_BUDGET
        );
        // …and the setup phase cannot borrow from the conversation, which is the
        // property that makes the sum above an upper bound rather than a hope.
        assert!(AGENT_SETUP_BUDGET < WORST_CASE_TOTAL);
        assert!(SSH_ADD_TIMEOUT < WORST_CASE_TOTAL);
        // The overrun is the engine's, not a second copy of it: a change to
        // `GH_CAPTURE_REAP_TIMEOUT` has to move this number rather than quietly
        // falsify it.
        assert_eq!(SETUP_STEP_OVERRUN, loomux_engine::subproc::GH_CAPTURE_REAP_TIMEOUT);
    }

    // ---- the setup phase's shape, on fake steps ----

    /// The fake phase's shared budget, and how long each fake step overruns the
    /// deadline it is handed — which is what a bounded capture does when it has
    /// to reap the child it killed.
    ///
    /// **Both are sized against a failure mode, not picked.** The first draft
    /// used 30 ms and 200 ms, and each had a defect a green run cannot show you
    /// (#2661 review, rev-std 1 and rev-final W2):
    ///
    /// - The BUDGET is the window in which the first step must be STARTED. At
    ///   30 ms, a scheduler stall of 30 ms between `Instant::now()` and
    ///   `run_setup_steps`'s first clock read leaves `left` zero, starts nothing,
    ///   and reddens the start-count assert with nothing wrong. Half a second is
    ///   beyond any plausible preemption on a CI runner, and a correct phase
    ///   still costs only budget + one overrun.
    /// - The OVERRUN sets what the un-fixed code costs, and the ceiling has to
    ///   sit strictly below that or the clock assert is a coin flip under the
    ///   very mutation it exists for. At 30 + 200 + 400 the ceiling was 630 ms
    ///   and the un-fixed three-step cost was also ~630 ms — the same number, so
    ///   only the FOUR-step arm ever separated.
    const PHASE_BUDGET: Duration = Duration::from_millis(500);
    const PHASE_OVERRUN: Duration = Duration::from_millis(500);

    /// Tolerance for a CI runner whose `sleep` is not a metronome. It is what is
    /// left over once the ceiling has cleared the un-fixed cost, not a number
    /// chosen first: 500 + 500 + 400 = 1400 ms, 600 ms clear of the 2000 ms an
    /// un-fixed three-step phase takes.
    const PHASE_SLACK: Duration = Duration::from_millis(400);

    /// Run `count` steps that each overrun whatever deadline they are given by
    /// `overrun` — which is exactly what a bounded capture does when it has to
    /// reap the child it killed. Returns how many were STARTED and how long the
    /// phase took.
    fn overrunning_phase(count: usize, budget: Duration, overrun: Duration) -> (usize, Duration) {
        let started = std::cell::Cell::new(0usize);
        let mut steps: Vec<Box<dyn FnMut(Duration) -> bool + '_>> = (0..count)
            .map(|_| {
                let step: Box<dyn FnMut(Duration) -> bool + '_> = Box::new(|left: Duration| {
                    started.set(started.get() + 1);
                    std::thread::sleep(left + overrun);
                    false
                });
                step
            })
            .collect();
        let began = Instant::now();
        let settled = run_setup_steps(began + budget, &mut steps);
        let elapsed = began.elapsed();
        assert!(!settled, "no fake step reports success, so the phase must not claim one");
        (started.get(), elapsed)
    }

    #[test]
    fn a_fourth_setup_step_costs_no_more_than_the_third() {
        // #2594 item 2. `WORST_CASE_TOTAL` carries ONE `SETUP_STEP_OVERRUN`, not
        // one per step, and the only thing that makes that true is that
        // `run_setup_steps` re-reads the shared deadline BETWEEN steps: a step
        // that would start past it is not started at all.
        //
        // Pinned by adding a step rather than by arithmetic over a count, which
        // is the shape #2397 review B1 removed from this module — a count is
        // exactly what a fourth step falsifies in silence.
        let (three_started, _) = overrunning_phase(3, PHASE_BUDGET, PHASE_OVERRUN);
        let (four_started, _) = overrunning_phase(4, PHASE_BUDGET, PHASE_OVERRUN);

        assert_eq!(three_started, 1, "only the step that had time left may run");
        assert_eq!(four_started, three_started, "a fourth step must not add a fourth start");
    }

    #[test]
    fn the_setup_phase_costs_one_overrun_however_many_steps_it_has() {
        // The CLOCK witness for the test above, and split out of it (#2661
        // review, rev-final W2) rather than left as three more asserts at the
        // end of it.
        //
        // A red evidences only the assertion it REACHED. Both witnesses fail
        // under the same mutation, and the start-count assert runs first, so
        // while they shared a test the clock assert had never been observed
        // failing at all — its message could not reach a log, and "the clock
        // says the same thing the start count does" was a claim with no round
        // behind it. Separated, one neuter reddens two tests and each says its
        // own thing.
        //
        // Correct, a phase costs budget + ONE overrun whatever its step count:
        // 500 + 500 = 1000 ms. Un-fixed, every step is started and every step
        // overruns, so N steps cost budget + N x overrun — 2000 ms for three and
        // 2500 ms for four. The ceiling has to separate 1000 from 2000.
        let (_, three_took) = overrunning_phase(3, PHASE_BUDGET, PHASE_OVERRUN);
        let (_, four_took) = overrunning_phase(4, PHASE_BUDGET, PHASE_OVERRUN);

        let ceiling = PHASE_BUDGET + PHASE_OVERRUN + PHASE_SLACK;
        // The property the two asserts below rest on, pinned rather than left in
        // a comment: a later edit that shrinks the overrun makes the ceiling
        // meet the un-fixed cost again and this test goes red, instead of going
        // quietly vacuous the way it did the first time (the ceiling was 630 ms
        // against an un-fixed 630 ms — the same number).
        assert!(
            ceiling < PHASE_BUDGET + PHASE_OVERRUN * 3,
            "the ceiling must sit below the un-fixed three-step cost, or the asserts below \
             are a coin flip: ceiling {ceiling:?} vs un-fixed {:?}",
            PHASE_BUDGET + PHASE_OVERRUN * 3
        );
        assert!(three_took < ceiling, "three steps took {three_took:?}, ceiling {ceiling:?}");
        assert!(four_took < ceiling, "four steps took {four_took:?}, ceiling {ceiling:?}");
    }

    #[test]
    fn every_step_runs_while_there_is_time_and_the_first_success_ends_the_phase() {
        // The positive control for the test above, which asserts that steps do
        // NOT run — an assertion a `run_setup_steps` that returned immediately
        // would satisfy just as well, taking the agent probe with it.
        let started = std::cell::Cell::new(0usize);
        let mut steps: Vec<Box<dyn FnMut(Duration) -> bool + '_>> = (0..4)
            .map(|_| {
                let step: Box<dyn FnMut(Duration) -> bool + '_> = Box::new(|_left: Duration| {
                    started.set(started.get() + 1);
                    false
                });
                step
            })
            .collect();
        assert!(!run_setup_steps(Instant::now() + Duration::from_secs(30), &mut steps));
        assert_eq!(started.get(), 4, "with time left, every step runs");

        // …and the first step that settles the question ends the phase, which is
        // what makes the agent probe stop at the first agent it finds.
        let started = std::cell::Cell::new(0usize);
        let counter = &started;
        let mut steps: Vec<Box<dyn FnMut(Duration) -> bool + '_>> = (0..4)
            .map(|i| {
                let step: Box<dyn FnMut(Duration) -> bool + '_> = Box::new(move |_left: Duration| {
                    counter.set(counter.get() + 1);
                    i == 1
                });
                step
            })
            .collect();
        assert!(run_setup_steps(Instant::now() + Duration::from_secs(30), &mut steps));
        assert_eq!(started.get(), 2, "the phase stops at the step that settled it");
    }

    // ---- a failed write is a refusal, on EVERY answer ----

    /// A console that has gone away: every write and every flush fails.
    #[derive(Debug)]
    struct BrokenConsole;
    impl std::io::Write for BrokenConsole {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the console went away"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the console went away"))
        }
    }

    /// A console that takes everything — the control for every assertion about
    /// a refusal, which an implementation that refused unconditionally would
    /// satisfy just as well.
    #[derive(Debug)]
    struct WorkingConsole;
    impl std::io::Write for WorkingConsole {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_re_send_surfaces_a_failed_write_instead_of_dropping_it() {
        // #2594 item 3. The re-send was `let _ = send_answer(…)`, so a console
        // that went away mid-conversation was reported as a fifteen-second
        // `Timeout` — the exact failure `send_answer`'s own doc says a dropped
        // result caused the first time round. It was the last write on this path
        // still dropping its result.
        let mut broken: Box<dyn std::io::Write + Send> = Box::new(BrokenConsole);
        let mut conv = Conversation::new();
        conv.answered = true;
        match conv.rearm(&mut broken, b"hunter2") {
            Some(SshAddOutcome::Failed { detail }) => {
                assert!(detail.contains("could not answer ssh-add"), "got: {detail}");
                assert!(detail.contains("the console went away"), "the cause survives: {detail}");
            }
            other => panic!("a failed re-send must refuse, got {other:?}"),
        }

        // The control: the same re-send over a live console decides nothing, so
        // this cannot be passed by refusing every run.
        let mut working: Box<dyn std::io::Write + Send> = Box::new(WorkingConsole);
        let mut conv = Conversation::new();
        conv.answered = true;
        assert_eq!(conv.rearm(&mut working, b"hunter2"), None);
        assert!(conv.rearmed, "…and the one permitted re-send is spent either way");
    }

    #[test]
    fn the_first_answer_surfaces_a_failed_write_too() {
        // The sibling of the test above, on the path that always surfaced it.
        // Pinned here because `step` is where both answers now go through the
        // same mapping — the reason there is one place left to drop a result.
        let mut broken: Box<dyn std::io::Write + Send> = Box::new(BrokenConsole);
        let mut conv = Conversation::new();
        conv.seen.push_str("Enter passphrase for /k/id: ");
        match conv.step(&mut broken, b"hunter2") {
            Some(SshAddOutcome::Failed { detail }) => {
                assert!(detail.contains("could not answer ssh-add"), "got: {detail}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }

        // Over a live console the same ask is answered and decides nothing…
        let mut working: Box<dyn std::io::Write + Send> = Box::new(WorkingConsole);
        let mut conv = Conversation::new();
        conv.seen.push_str("Enter passphrase for /k/id: ");
        assert_eq!(conv.step(&mut working, b"hunter2"), None);
        assert!(conv.answered);
        // …and the ask is consumed, which is what makes it decidable once and
        // puts the tail's start mid-line for the anchor above.
        assert_eq!(conv.consumed, conv.seen.len());
    }

    #[test]
    fn zeroing_actually_clears_the_buffer() {
        let mut buf = b"hunter2".to_vec();
        zero_secret(&mut buf);
        assert!(buf.iter().all(|b| *b == 0));
    }
}
