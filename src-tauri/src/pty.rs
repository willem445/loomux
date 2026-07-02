//! PTY management on top of portable-pty (WezTerm's PTY layer).
//! Uses ConPTY on Windows and forkpty on Unix, so escape sequences,
//! colors, and wide characters behave exactly as a native terminal.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

pub struct PtyHandle {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

#[derive(Default)]
pub struct PtyManager {
    ptys: Arc<Mutex<HashMap<u32, PtyHandle>>>,
    next_id: AtomicU32,
}

impl PtyManager {
    /// Kill every child process; used on app shutdown so shells (and any
    /// agents running in them) don't outlive the window.
    pub fn kill_all(&self) {
        let handles: Vec<_> = self.ptys.lock().unwrap().drain().collect();
        for (_, mut h) in handles {
            let _ = h.killer.kill();
        }
    }
}

#[derive(Clone, Serialize)]
struct ExitPayload {
    id: u32,
    exit_code: Option<u32>,
}

#[derive(Clone, Serialize)]
struct OutputPayload {
    id: u32,
    /// Base64-encoded raw bytes so the transport is lossless.
    data: String,
}

/// Pick the user's default interactive shell.
fn default_shell() -> (String, Vec<String>) {
    #[cfg(target_os = "windows")]
    {
        // Prefer PowerShell 7 when available, fall back to Windows PowerShell.
        for candidate in ["pwsh.exe", "powershell.exe"] {
            if which(candidate) {
                return (candidate.to_string(), vec!["-NoLogo".to_string()]);
            }
        }
        ("cmd.exe".to_string(), vec![])
    }
    #[cfg(not(target_os = "windows"))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        (shell, vec!["-l".to_string()])
    }
}

#[cfg(target_os = "windows")]
fn which(name: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

/// Build the argv for a pane. When `command` is given it is run *through*
/// the default shell so PATH shims (.cmd/.ps1 wrappers like `claude`)
/// resolve the same way they do in a normal terminal.
fn build_command(command: Option<String>, cwd: Option<String>) -> CommandBuilder {
    let (shell, shell_args) = default_shell();
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
            for a in &shell_args {
                cmd.arg(a);
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
    state.ptys.lock().unwrap().insert(
        id,
        PtyHandle {
            master: pair.master,
            writer,
            killer,
        },
    );

    // Reader thread: stream output on a single shared channel keyed by id.
    // The frontend router buffers payloads for panes that haven't attached
    // their handler yet, so no output can be lost at startup.
    let out_app = app.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
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

    // Waiter thread: reap the child, then tear down and notify.
    let ptys = state.ptys.clone();
    std::thread::spawn(move || {
        let status = child.wait();
        ptys.lock().unwrap().remove(&id);
        let exit_code = status.ok().map(|s| s.exit_code());
        let _ = app.emit("pty-exit", ExitPayload { id, exit_code });
    });

    Ok(id)
}

#[tauri::command]
pub fn write_pty(state: State<PtyManager>, id: u32, data: String) -> Result<(), String> {
    let mut ptys = state.ptys.lock().unwrap();
    let pty = ptys.get_mut(&id).ok_or("pty not found")?;
    pty.writer
        .write_all(data.as_bytes())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn resize_pty(state: State<PtyManager>, id: u32, cols: u16, rows: u16) -> Result<(), String> {
    let ptys = state.ptys.lock().unwrap();
    let pty = ptys.get(&id).ok_or("pty not found")?;
    pty.master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn kill_pty(state: State<PtyManager>, id: u32) -> Result<(), String> {
    // Remove first so the handle (and its master side) drops; then signal
    // the child. The waiter thread emits pty-exit once it reaps.
    let handle = state.ptys.lock().unwrap().remove(&id);
    if let Some(mut h) = handle {
        let _ = h.killer.kill();
    }
    Ok(())
}
