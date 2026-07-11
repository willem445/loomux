//! Native-style file MANAGER backend (issue #214).
//!
//! This is the Explorer/Finder/Nautilus equivalent, NOT the in-app editor: you
//! browse folders, and double-clicking a file hands it to **the OS default
//! application for its extension**. Loomux never opens it. That single sentence is
//! what separates this module from `fileedit` — which is the in-app editor
//! (`Alt+F`), still there, still unchanged, and deliberately not what a
//! file-explorer pane hosts.
//!
//! ## Path safety
//!
//! Every command takes a `root` (the pane's folder) and a `rel` *relative to it*,
//! and routes through `fileedit::safe_resolve` — the module's existing, tested
//! choke point (lexical `..` folding, no absolute `rel`, no traversal *through* a
//! symlink). It is reused rather than reimplemented on purpose: a second
//! path-validation implementation is a second one to get wrong, and this module
//! DELETES things. The webview is trusted, but these operations are destructive
//! and sit next to agent-facing surfaces, so the check is defense-in-depth exactly
//! as CLAUDE.md constraint #6 asks (`fileedit`'s own header makes the same
//! argument for reads and writes).
//!
//! On top of that, every *name* the user supplies (new folder, rename target) must
//! be a single plain path segment — `validate_name` rejects separators, `.`/`..`,
//! and the Windows reserved device names — so a rename can never relocate a file,
//! only re-label it.
//!
//! ## No new crates
//!
//! Two Windows Shell APIs from the `windows` crate we ALREADY depend on:
//!
//!   * `ShellExecuteW` — "open with the default app", exactly what Explorer's
//!     double-click does. It takes a path, never a shell string, so unlike
//!     `cmd /c start` there is nothing to re-parse and no injection surface.
//!   * `SHFileOperationW` with `FO_DELETE | FOF_ALLOWUNDO` — delete to the
//!     **Recycle Bin**, so a mis-click is recoverable.
//!
//! The `trash` crate would have covered the second, and `tauri-plugin-opener` the
//! first, but both are unvetted additions to a tree with a hard getrandom ban
//! (CLAUDE.md constraint #2 — `bcryptprimitives.dll!ProcessPrng` isn't exported on
//! this project's Windows 10 baseline, and the binary then fails to load with
//! 0xc0000139). Reaching for an API we already link sidesteps the question
//! entirely: `cargo tree -i getrandom` is unchanged by this module.
//!
//! On macOS/Linux the default-app open is `open`/`xdg-open` (spawned detached,
//! argv — never a shell string), and there is **no Recycle Bin**: delete is
//! permanent. `fm_delete_mode` reports which, so the confirmation dialog can say
//! what will actually happen instead of guessing.

use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::fileedit::{err, safe_resolve};

/// Does any component of `rel` name something Windows would silently MANGLE?
///
/// The Win32 path layer strips trailing spaces and dots from each component before
/// it hits the filesystem, so `root\"   "` and `root\"sub."` open `root` and
/// `root\sub`. Rust's `PathBuf` **preserves** those characters, which is the trap:
/// `root.join("   ")` compares unequal to `root`, so a resolved-path check says
/// "that's a child" while the OS goes on to operate on the parent. Comparing
/// resolved paths therefore cannot catch this — it has to be a check on the
/// components themselves, before they are ever handed to the filesystem.
///
/// `.` and `..` are deliberately exempt: they are real, meaningful components that
/// `safe_resolve`'s lexical normalizer folds (and then bounds against the root).
fn has_mangled_component(rel: &str) -> bool {
    rel.split(['/', '\\']).any(|c| {
        if c.is_empty() || c == "." || c == ".." {
            return false;
        }
        c.trim().is_empty() || c.ends_with(' ') || c.ends_with('.')
    })
}

/// Resolve `rel` under `root`, refusing anything Windows would mangle first. Every
/// command in this module goes through here or through `resolve_child`.
fn resolve(root: &str, rel: &str) -> Result<PathBuf, String> {
    if has_mangled_component(rel) {
        return Err(err(
            "invalid-path",
            "a path component cannot be blank or end with a space or a dot",
        ));
    }
    safe_resolve(root, rel)
}

/// Resolve `rel` to a path strictly INSIDE the root — never the root itself.
///
/// `delete` and `rename` act *on* an entry, and that entry must never be the pane's
/// own root folder. Two distinct ways to land on the root, and both are covered
/// because the first implementation of this fell to the second:
///
///   * `""`, `"."`, `"sub/.."` — these genuinely *resolve* to the root, so the
///     resolved-path comparison below catches them;
///   * `"   "`, `"sub."` — these do NOT resolve to the root as far as Rust is
///     concerned, but the OS strips them and operates on it anyway. `resolve`'s
///     component check is what stops those, and the integration test that found it
///     had `delete` sending the pane's own root to the Recycle Bin.
fn resolve_child(root: &str, rel: &str) -> Result<PathBuf, String> {
    let path = resolve(root, rel)?;
    if path == resolve(root, "")? {
        return Err(err(
            "invalid-path",
            "refusing to act on the root folder itself",
        ));
    }
    Ok(path)
}

/// Windows reserved device names. Creating `CON`, `NUL`, `COM1`… (with or without
/// an extension) is refused by the OS in confusing ways, so we reject them up
/// front with a legible message. Checked on every platform: a repo created on
/// Linux with a file called `aux.txt` becomes un-checkoutable on Windows, so
/// refusing to *create* one here is a kindness, not a portability bug.
const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Characters no name may contain. The path separators are the load-bearing ones
/// (they are what would turn a "rename" into a "move"); the rest are illegal on
/// Windows anyway and fail confusingly at the syscall if we let them through.
const ILLEGAL_NAME_CHARS: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'];

// ---------- wire types ----------

/// One row in a directory listing. Everything the manager renders comes from here.
///
/// Deliberately NOT sorted or filtered by the backend: the ordering (folders
/// first, then case-insensitive by name) and the hidden-files filter are pure
/// decisions, so they live in the frontend where they are unit-tested without a
/// DOM (`src/fileexplorermodel.ts`).
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct FmEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Bytes. 0 for directories and symlinks (we never follow one to size it).
    pub size: u64,
    /// Last-modified, milliseconds since the Unix epoch. 0 when unavailable —
    /// rendered as "—" rather than as 1970.
    pub modified_ms: u64,
    /// Platform-correct hidden flag: the `FILE_ATTRIBUTE_HIDDEN` bit on Windows,
    /// a leading dot elsewhere. Computed here because only the backend can know;
    /// whether to *show* hidden entries is the frontend's (tested) choice.
    pub is_hidden: bool,
}

/// What `fm_delete` will actually do on this platform, so the confirmation can say
/// so. "recycle" → recoverable from the Recycle Bin; "permanent" → gone.
#[derive(Serialize, Debug, PartialEq)]
pub struct DeleteMode {
    pub mode: &'static str,
}

// ---------- names ----------

/// Validate a user-supplied entry name (new folder, rename target). Returns the
/// trimmed name, or a typed error.
///
/// The separator rejection is the security-relevant one: without it a "rename" to
/// `../../elsewhere` would be a *move* out of the root. `safe_resolve` would still
/// catch that downstream, but failing here gives the user a sentence they can act
/// on ("names can't contain \\ or /") instead of an opaque containment error.
pub fn validate_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(err("invalid-name", "name cannot be empty"));
    }
    if name == "." || name == ".." {
        return Err(err("invalid-name", "name cannot be '.' or '..'"));
    }
    if let Some(bad) = name.chars().find(|c| ILLEGAL_NAME_CHARS.contains(c)) {
        return Err(err(
            "invalid-name",
            format!("name cannot contain '{bad}'"),
        ));
    }
    // Windows silently STRIPS a trailing dot, so `fs::create_dir("foo.")` gives you
    // `foo` — a name that is not the one the user typed. Reject rather than
    // surprise. (Trailing spaces are the same hazard, but the trim above already
    // removed them, so there is nothing left here to check for.)
    if name.ends_with('.') {
        return Err(err("invalid-name", "name cannot end with a dot"));
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    if RESERVED_NAMES.contains(&stem.as_str()) {
        return Err(err(
            "invalid-name",
            format!("'{name}' is a reserved device name on Windows"),
        ));
    }
    Ok(name.to_string())
}

/// Join a validated `name` onto a `rel` directory, in the forward-slashed `rel`
/// convention the rest of the file stack uses.
fn join_rel(rel: &str, name: &str) -> String {
    let rel = rel.trim_matches('/');
    if rel.is_empty() {
        name.to_string()
    } else {
        format!("{rel}/{name}")
    }
}

/// The parent directory of `rel`, in `rel` terms ("" = the root itself).
fn parent_rel(rel: &str) -> String {
    match rel.trim_matches('/').rfind('/') {
        Some(i) => rel[..i].to_string(),
        None => String::new(),
    }
}

// ---------- listing ----------

#[cfg(windows)]
fn is_hidden(path: &Path, md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    let _ = path;
    md.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(windows))]
fn is_hidden(path: &Path, _md: &std::fs::Metadata) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.'))
        .unwrap_or(false)
}

/// Milliseconds since the Unix epoch for a file's mtime; 0 when unavailable (a
/// filesystem without mtimes, or a clock before 1970 — either way "unknown", which
/// the UI renders as "—").
fn modified_ms(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// List one directory under `root`. Unsorted and unfiltered — see `FmEntry`.
///
/// A symlink is reported as a symlink and never followed (so `size`/`is_dir`
/// describe the LINK, not its target): the manager shows it, and `safe_resolve`
/// refuses to traverse through it, which together mean a symlinked directory can't
/// smuggle the user (or an operation) outside the root.
pub fn list(root: &str, rel: &str) -> Result<Vec<FmEntry>, String> {
    let dir = resolve(root, rel)?;
    let md = std::fs::symlink_metadata(&dir).map_err(|e| err("not-found", e.to_string()))?;
    if !md.is_dir() {
        return Err(err("not-dir", format!("not a directory: {rel}")));
    }
    let mut out = Vec::new();
    for ent in std::fs::read_dir(&dir).map_err(|e| err("io", e.to_string()))? {
        let ent = match ent {
            Ok(e) => e,
            Err(_) => continue, // one unreadable entry must not fail the whole listing
        };
        let path = ent.path();
        // Own-type metadata: a symlink reports as a symlink here, not as its target.
        let md = match std::fs::symlink_metadata(&path) {
            Ok(md) => md,
            Err(_) => continue,
        };
        let is_symlink = md.file_type().is_symlink();
        let is_dir = !is_symlink && md.is_dir();
        out.push(FmEntry {
            name: ent.file_name().to_string_lossy().into_owned(),
            is_dir,
            is_symlink,
            size: if is_dir || is_symlink { 0 } else { md.len() },
            modified_ms: modified_ms(&md),
            is_hidden: is_hidden(&path, &md),
        });
    }
    Ok(out)
}

// ---------- operations ----------

/// Create a directory named `name` inside `rel`. Returns the new entry's `rel`.
/// Refuses to clobber: an existing name is an error, never a silent no-op.
pub fn new_folder(root: &str, rel: &str, name: &str) -> Result<String, String> {
    let name = validate_name(name)?;
    let child = join_rel(rel, &name);
    let path = resolve(root, &child)?;
    if path.exists() {
        return Err(err("exists", format!("'{name}' already exists")));
    }
    // create_dir, NOT create_dir_all: `name` is one validated segment, so there is
    // no parent chain to build — and _all would happily mask a typo'd `rel`.
    std::fs::create_dir(&path).map_err(|e| err("io", e.to_string()))?;
    Ok(child)
}

/// Rename the entry at `rel` to `name`, in place. Returns the new `rel`.
///
/// `name` is one validated segment, so this can only re-label — never move. The
/// destination is resolved through the same choke point and must not already
/// exist (`fs::rename` would otherwise overwrite a file silently on Unix).
pub fn rename(root: &str, rel: &str, name: &str) -> Result<String, String> {
    let name = validate_name(name)?;
    // Same guard as delete: renaming the pane's own root is not a thing.
    let from = resolve_child(root, rel)?;
    if !from.exists() {
        return Err(err("not-found", format!("'{rel}' no longer exists")));
    }
    let target = join_rel(&parent_rel(rel), &name);
    let to = resolve(root, &target)?;
    if to == from {
        return Ok(target); // renamed to itself — nothing to do, not an error
    }
    // Guard the clobber explicitly. `fs::rename` overwrites an existing file on
    // Unix without a word; losing a file to a rename typo is exactly the kind of
    // thing a file manager must not do.
    if to.exists() {
        return Err(err("exists", format!("'{name}' already exists")));
    }
    std::fs::rename(&from, &to).map_err(|e| err("io", e.to_string()))?;
    Ok(target)
}

/// Delete the entry at `rel` — to the Recycle Bin on Windows, permanently
/// elsewhere. Returns whether it was recycled (recoverable) or destroyed.
pub fn delete(root: &str, rel: &str) -> Result<bool, String> {
    // resolve_child, not safe_resolve: it is what refuses to delete the ROOT, and it
    // does so on the resolved path because a lexical check cannot (see its doc).
    let path = resolve_child(root, rel)?;
    let md = std::fs::symlink_metadata(&path).map_err(|e| err("not-found", e.to_string()))?;
    delete_path(&path, md.is_dir() && !md.file_type().is_symlink())
}

#[cfg(windows)]
fn delete_path(path: &Path, _is_dir: bool) -> Result<bool, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
        FO_DELETE, SHFILEOPSTRUCTW,
    };

    // SHFileOperationW takes a DOUBLE-nul-terminated list of paths. One path here,
    // so: <path> NUL NUL. Getting this wrong reads past the buffer, so it is worth
    // being explicit about.
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.push(0);
    from.push(0);

    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE as u32,
        pFrom: windows::core::PCWSTR(from.as_ptr()),
        // ALLOWUNDO is the Recycle Bin. The other three suppress Explorer's own UI:
        // loomux has already asked the user, and a modal from the shell on top of
        // our own confirmation would be both redundant and (being another window)
        // a focus-stealing surprise.
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    // SAFETY: `from` is a double-nul-terminated UTF-16 buffer that outlives the
    // call, and `op` is fully initialized. SHFileOperationW returns 0 on success;
    // its error codes are its own (not GetLastError), hence the bare code in the
    // message — it is for a bug report, not for the user, who sees the prose.
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 {
        return Err(err(
            "io",
            format!("the shell refused to delete this item (code {rc})"),
        ));
    }
    // fAnyOperationsAborted is set when the user cancelled a shell prompt. We
    // suppressed those, so this really means "the shell declined" — report it
    // rather than claim success on an item that is still there.
    if op.fAnyOperationsAborted.as_bool() {
        return Err(err("io", "the delete was aborted"));
    }
    Ok(true) // recycled
}

#[cfg(not(windows))]
fn delete_path(path: &Path, is_dir: bool) -> Result<bool, String> {
    // No Recycle Bin without a new dependency, and the getrandom ban makes adding
    // one a research project. So: permanent — and `fm_delete_mode` tells the UI, so
    // the confirmation says "permanently delete" and doesn't promise an undo that
    // doesn't exist.
    if is_dir {
        std::fs::remove_dir_all(path).map_err(|e| err("io", e.to_string()))?;
    } else {
        std::fs::remove_file(path).map_err(|e| err("io", e.to_string()))?;
    }
    Ok(false) // permanently deleted
}

/// Whether `delete` recycles or destroys on this platform.
pub fn delete_mode() -> DeleteMode {
    DeleteMode {
        mode: if cfg!(windows) { "recycle" } else { "permanent" },
    }
}

// ---------- open with the OS default application ----------

/// Hand `rel` to the operating system's default application for its extension —
/// the whole point of the feature, and exactly what a double-click in Explorer
/// does. Loomux does not interpret the file, does not read it, and has no opinion
/// about its type.
///
/// A DIRECTORY is refused: navigating into a folder is the manager's own job, and
/// handing one to the shell would pop a second, separate Explorer window — the
/// exact thing the issue exists to stop the user needing.
pub fn open_default(root: &str, rel: &str) -> Result<(), String> {
    let path = resolve(root, rel)?;
    let md = std::fs::symlink_metadata(&path).map_err(|e| err("not-found", e.to_string()))?;
    if md.is_dir() {
        return Err(err("is-dir", "directories are navigated, not opened"));
    }
    open_os(&path)
}

#[cfg(windows)]
fn open_os(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = "open\0".encode_utf16().collect();

    // SAFETY: both buffers are nul-terminated and outlive the call. ShellExecuteW
    // takes the path as a PATH — it is not a command line and is never re-parsed,
    // so a filename full of spaces, quotes or ampersands is inert here. (This is
    // the reason to prefer it over `cmd /c start "" <path>`, which hands the name
    // to a shell.)
    let rc = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    // Legacy API: "success" is an HINSTANCE > 32. The two failures worth naming are
    // the ones a user can actually act on.
    let code = rc.0 as usize;
    if code > 32 {
        return Ok(());
    }
    Err(match code {
        // SE_ERR_NOASSOC / ERROR_NO_ASSOCIATION
        31 | 1155 => err(
            "no-assoc",
            "no default app is associated with this file type",
        ),
        2 | 3 => err("not-found", "the file could not be found"),
        _ => err("io", format!("the shell could not open this file (code {code})")),
    })
}

#[cfg(not(windows))]
fn open_os(path: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};
    // argv, never a shell string — same guarantee as the Windows arm and as
    // `editor.rs`: a filename with spaces or metacharacters stays ONE argument and
    // can never be reinterpreted as syntax.
    let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    Command::new(program)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| err("io", format!("could not launch {program}: {e}")))
}

// ---------- commands ----------

#[tauri::command]
pub fn fm_list(root: String, rel: String) -> Result<Vec<FmEntry>, String> {
    list(&root, &rel)
}

#[tauri::command]
pub fn fm_new_folder(root: String, rel: String, name: String) -> Result<String, String> {
    new_folder(&root, &rel, &name)
}

#[tauri::command]
pub fn fm_rename(root: String, rel: String, name: String) -> Result<String, String> {
    rename(&root, &rel, &name)
}

#[tauri::command]
pub fn fm_delete(root: String, rel: String) -> Result<bool, String> {
    delete(&root, &rel)
}

#[tauri::command]
pub fn fm_delete_mode() -> DeleteMode {
    delete_mode()
}

#[tauri::command]
pub fn fm_open(root: String, rel: String) -> Result<(), String> {
    open_default(&root, &rel)
}
