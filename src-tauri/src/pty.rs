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
const OUTPUT_RING_CAP: usize = 256 * 1024;

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

pub struct PtyHandle {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
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
    /// Whether a human's typed line is currently sitting UNSUBMITTED in this
    /// pane's input box (#111). Tracked from the *content* of each human write,
    /// not from output bytes: printable input sets it (the box now holds a line),
    /// an Enter / line-clear resets it (the line was submitted or cleared). This
    /// positive submit/clear signal is what lets prompt delivery hold a paste off
    /// a human's half-written line without wedging on an already-submitted one —
    /// output-byte heuristics can't tell a keystroke's echo from a submit burst.
    input_pending: Arc<std::sync::atomic::AtomicBool>,
    /// The interactive shell this pane *effectively* spawned (#194 P2) — after
    /// any discovery-miss fallback, not the requested kind. Recorded so the
    /// folder-picker `cd` (`change_dir`) emits the pane's own shell syntax — cmd,
    /// PowerShell, or Git Bash — instead of guessing from the machine default
    /// (rev-78 #3, nit 3). Agent/custom panes record PowerShell (or its cmd
    /// degrade); they don't drive the folder picker.
    shell_kind: ShellKind,
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

    /// Raw write into a pty's stdin; used by orchestration to type prompts
    /// into agent CLIs so the human sees them verbatim.
    pub fn write_bytes(&self, id: u32, bytes: &[u8]) -> Result<(), String> {
        let mut ptys = self.ptys.lock_safe();
        let pty = ptys.get_mut(&id).ok_or("pty not found")?;
        pty.writer.write_all(bytes).map_err(|e| e.to_string())
    }

    /// Snapshot of the rolling output tail (raw bytes, ANSI included).
    pub fn output_tail(&self, id: u32) -> Option<Vec<u8>> {
        let ptys = self.ptys.lock_safe();
        let buf = ptys.get(&id)?.output.lock_safe();
        Some(buf.ring.iter().copied().collect())
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
        Some(ptys.get(&id)?.input_pending.load(Ordering::Relaxed))
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
    pub fn kill(&self, id: u32) {
        self.expected_exits.lock_safe().insert(id);
        let handle = self.ptys.lock_safe().remove(&id);
        if let Some(mut h) = handle {
            let _ = h.killer.kill();
        }
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
#[tauri::command]
pub fn discover_git_bash() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        find_git_bash().map(|p| p.to_string_lossy().into_owned())
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
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

/// Resolve `.`/`..` in a path lexically, without touching the filesystem. Kept
/// off `fs::canonicalize` on purpose: that returns a `\\?\`-verbatim path on
/// Windows, which some toolchains mishandle in env vars. Inputs here are real
/// absolute paths, so a lexical fold is sufficient and deterministic (testable).
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = Vec::new();
    for comp in p.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out.iter().collect()
}

/// If `cwd` is a **linked git worktree**, return the shared per-repo cargo
/// target dir to point `CARGO_TARGET_DIR` at — `<main-repo-root>/.loomux-target`
/// — so every agent worktree reuses ONE build cache instead of each paying a
/// fresh 5–7 GB `target/` (#134). Returns `None` for a normal checkout (whose
/// `.git` is a directory), so the main repo keeps its own `target/`.
///
/// Pure filesystem inspection — no `git` subprocess: a linked worktree's `.git`
/// is a *file* `gitdir: <main>/.git/worktrees/<id>`; that dir's `commondir`
/// resolves to the main repo's `.git`, whose parent is the shared root. Kept
/// pure so the mapping is unit-testable against a fixture tree.
#[doc(hidden)] // pub for the worktree-target integration test
pub fn shared_worktree_target_dir(cwd: &Path) -> Option<PathBuf> {
    // Opt-out escape hatch, mirroring LOOMUX_NO_DIRECT_SPAWN: a one-env-var
    // rollback if the shared cache ever misbehaves (a worktree then builds its
    // own target/ as before).
    if std::env::var_os("LOOMUX_NO_SHARED_TARGET").is_some() {
        return None;
    }
    let dot_git = cwd.join(".git");
    if !std::fs::metadata(&dot_git).ok()?.is_file() {
        return None; // real checkout (dir) or no repo → keep the normal target/
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let worktree_gitdir = PathBuf::from(gitdir);
    let commondir = std::fs::read_to_string(worktree_gitdir.join("commondir")).ok()?;
    let commondir = commondir.trim();
    let common = if Path::new(commondir).is_absolute() {
        PathBuf::from(commondir)
    } else {
        worktree_gitdir.join(commondir)
    };
    // `common` is the main repo's `.git`; its parent is the repo root.
    let root = lexical_normalize(&common);
    let root = root.parent()?;
    Some(root.join(".loomux-target"))
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
        // #134: a pane whose cwd is a git worktree shares one cargo build cache
        // across all worktrees instead of each growing its own 5–7 GB target/.
        // Only linked worktrees get this; the main checkout keeps target/.
        // Respect an operator-set CARGO_TARGET_DIR (don't override a deliberate
        // choice).
        if std::env::var_os("CARGO_TARGET_DIR").is_none() {
            if let Some(target) = shared_worktree_target_dir(Path::new(dir)) {
                cmd.env("CARGO_TARGET_DIR", target);
            }
        }
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

#[tauri::command]
pub fn spawn_pty(
    app: AppHandle,
    state: State<PtyManager>,
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
    state.ptys.lock_safe().insert(
        id,
        PtyHandle {
            master: pair.master,
            writer,
            killer,
            output: output.clone(),
            user_input_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            #[cfg(target_os = "windows")]
            _job: job,
            input_pending: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            // Record what actually spawned, not what was requested, so the
            // folder-picker cd is truthful even after a discovery-miss fallback.
            shell_kind: effective_shell_kind(kind),
        },
    );

    crate::obs::breadcrumb("pty-open", &format!("id={id} cols={cols} rows={rows}"));

    // Reader thread: stream output on a single shared channel keyed by id.
    // The frontend router buffers payloads for panes that haven't attached
    // their handler yet, so no output can be lost at startup. A rolling tail
    // is teed into the ring for orchestration's `get_output`.
    let out_app = app.clone();
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
                    let _ = out_app.emit(
                        "pty-output",
                        OutputPayload {
                            id,
                            data: B64.encode(&buf[..n]),
                        },
                    );
                }
            }
        }
    });

    // Waiter thread: reap the child, then tear down and notify. Orchestration
    // learns about agent deaths here (authoritative, even if the frontend
    // never noticed the pane).
    let ptys = state.ptys.clone();
    let expected_exits = state.expected_exits.clone();
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

#[tauri::command]
pub fn write_pty(state: State<PtyManager>, id: u32, data: String) -> Result<(), String> {
    let mut ptys = state.ptys.lock_safe();
    let pty = ptys.get_mut(&id).ok_or("pty not found")?;
    pty.user_input_ms.store(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        Ordering::Relaxed,
    );
    // Track box occupancy from the keystroke's CONTENT (#111): printable input
    // leaves a line sitting in the box; an Enter / line-clear empties it. Neutral
    // edits (arrows, backspace, bare escape sequences) leave occupancy unchanged.
    match crate::orchestration::classify_human_input(&data) {
        crate::orchestration::HumanInput::Content => pty.input_pending.store(true, Ordering::Relaxed),
        crate::orchestration::HumanInput::Submit => pty.input_pending.store(false, Ordering::Relaxed),
        crate::orchestration::HumanInput::Neutral => {}
    }
    pty.writer
        .write_all(data.as_bytes())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn resize_pty(state: State<PtyManager>, id: u32, cols: u16, rows: u16) -> Result<(), String> {
    let ptys = state.ptys.lock_safe();
    let pty = ptys.get(&id).ok_or("pty not found")?;
    pty.master
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
#[tauri::command]
pub fn dir_info(path: String) -> DirInfo {
    let dir = Path::new(&path);
    DirInfo {
        cwd: abbreviate_home(dir),
        branch: git_branch(dir),
    }
}

/// Send a `cd` into a pane's shell, so the folder picker can drive it. The
/// command is formatted for the pane's *own* shell kind (#194 P2), not the
/// machine default — a cmd or Git Bash pane must not receive PowerShell syntax.
#[tauri::command]
pub fn change_dir(state: State<PtyManager>, id: u32, path: String) -> Result<(), String> {
    let mut ptys = state.ptys.lock_safe();
    let pty = ptys.get_mut(&id).ok_or("pty not found")?;
    let line = cd_command_line(&path, pty.shell_kind);
    pty.writer
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())
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

    /// Serializes the two tests that mutate the process-global
    /// `LOOMUX_NO_DIRECT_SPAWN` so they can't race each other's reads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// The whole direct-vs-shell decision (issue #78), sequenced in one test so
    /// the escape-hatch env mutation can't race sibling cases run in parallel.
    #[test]
    fn direct_spawn_selection() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
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
            !prog(&direct).contains("pwsh") && !prog(&direct).contains("sh"),
            "a direct spawn must not go through a shell"
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
}
