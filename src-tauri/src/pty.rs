//! PTY management on top of portable-pty (WezTerm's PTY layer).
//! Uses ConPTY on Windows and forkpty on Unix, so escape sequences,
//! colors, and wide characters behave exactly as a native terminal.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::obs::LockExt;

/// Cap on the per-pty output ring used by orchestration's `get_output` —
/// enough for a few screens of TUI history without unbounded growth.
const OUTPUT_RING_CAP: usize = 256 * 1024;

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

/// Build the argv for a pane. When `command` is given it is run *through*
/// the default shell so PATH shims (.cmd/.ps1 wrappers like `claude`)
/// resolve the same way they do in a normal terminal. A plain interactive
/// shell additionally gets cwd-reporting shell integration wired in.
fn build_command(command: Option<String>, cwd: Option<String>) -> CommandBuilder {
    let shell = default_shell();
    let mut cmd = CommandBuilder::new(&shell);

    match command {
        Some(line) if !line.trim().is_empty() => {
            #[cfg(target_os = "windows")]
            {
                if shell.contains("cmd.exe") {
                    cmd.args(["/C", &line]);
                } else {
                    cmd.args(["-NoLogo", "-Command", &line]);
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                cmd.args(["-lc", &line]);
            }
        }
        _ => {
            #[cfg(target_os = "windows")]
            {
                if shell.contains("cmd.exe") {
                    // cmd's PROMPT understands $E (ESC), $P (path), $G (>).
                    cmd.args(["/K", "prompt $E]7;$P$E\\$P$G"]);
                } else {
                    cmd.args(["-NoLogo", "-NoExit", "-Command", PWSH_CWD_HOOK]);
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                cmd.arg("-l");
                // bash emits OSC 7 before each prompt. (zsh ignores this.)
                cmd.env("PROMPT_COMMAND", "printf '\\033]7;%s\\007' \"$PWD\"");
            }
        }
    }

    let dir = cwd
        .filter(|d| std::path::Path::new(d).is_dir())
        .or_else(|| dirs::home_dir().map(|h| h.to_string_lossy().into_owned()));
    if let Some(dir) = dir {
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

#[tauri::command]
pub fn spawn_pty(
    app: AppHandle,
    state: State<PtyManager>,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
    command: Option<String>,
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

    let cmd = build_command(command, cwd);
    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);

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
        ptys.lock_safe().remove(&id);
        let expected = expected_exits.lock_safe().remove(&id);
        let exit_code = status.ok().map(|s| s.exit_code());
        crate::obs::breadcrumb(
            "pty-exit",
            &format!("id={id} code={exit_code:?} expected={expected}"),
        );
        if let Some(reg) = app.try_state::<Arc<crate::orchestration::OrchRegistry>>() {
            reg.on_pty_exit(id, exit_code);
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
/// command is formatted for the platform's default shell.
#[tauri::command]
pub fn change_dir(state: State<PtyManager>, id: u32, path: String) -> Result<(), String> {
    let line = cd_command_line(&path);
    let mut ptys = state.ptys.lock_safe();
    let pty = ptys.get_mut(&id).ok_or("pty not found")?;
    pty.writer
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())
}

/// Build a shell-appropriate `cd` command line (Enter-terminated) that
/// tolerates spaces and quotes in `path`.
fn cd_command_line(path: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        if default_shell().contains("cmd.exe") {
            format!("cd /d \"{path}\"\r")
        } else {
            // PowerShell: single-quote and double any embedded quotes.
            format!("Set-Location -LiteralPath '{}'\r", path.replace('\'', "''"))
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
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

    #[test]
    fn git_branch_walks_up_to_repo_root() {
        // The crate lives inside the loomux repo but has no `.git` of its own,
        // so this exercises the parent walk.
        let here = std::env::current_dir().unwrap();
        assert!(git_branch(&here).is_some());
    }
}
