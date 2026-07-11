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
use tauri::{AppHandle, Emitter};

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

/// Create an EMPTY file named `name` inside `rel`. Returns the new entry's `rel`.
///
/// Deliberately empty, and deliberately not opened: the user's next double-click is
/// what hands it to their default app. Creating it and launching an editor would be
/// loomux deciding what the file is for, which is the thing this pane exists not to
/// do.
///
/// `create_new(true)` is the load-bearing flag — it makes "create, but only if it
/// isn't there" ATOMIC. The `exists()` check above it is only there for the nicer
/// message; on its own it would be a TOCTOU window in which an agent (or the user's
/// own editor) could create the file between the check and the write, and we would
/// then truncate it to nothing. Same shape as `new_folder`'s `create_dir`, which is
/// likewise a refuse-if-present call rather than a `create_dir_all`.
pub fn new_file(root: &str, rel: &str, name: &str) -> Result<String, String> {
    let name = validate_name(name)?;
    let child = join_rel(rel, &name);
    let path = resolve(root, &child)?;
    if path.exists() {
        return Err(err("exists", format!("'{name}' already exists")));
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                err("exists", format!("'{name}' already exists"))
            } else {
                err("io", e.to_string())
            }
        })?;
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

/// An RAII COM apartment for the calling thread (issue #216).
///
/// **This is the whole reason `fm_delete` was not simply made an `async fn`.**
/// `SHFileOperationW` is a Shell API: the Shell is COM, and the documented contract is
/// that the calling thread has initialized COM — in practice a single-threaded apartment,
/// because the Shell's file-operation machinery is apartment-affine and (with UI enabled)
/// pumps messages.
///
/// Today it works *by accident of where it runs*: the Tauri main thread is a GUI thread
/// that `wry` has already `OleInitialize`d as an STA, so the requirement is satisfied by
/// someone else. Hand the call to a bare worker thread and that stops being true — the
/// worker inherits nothing. So the worker must enter its **own** apartment, and leave it
/// again, exactly once. That is what this guard is.
///
/// The three return values all mean different things and all matter:
///
///   * `S_OK`     — we initialized the apartment. We must uninitialize it.
///   * `S_FALSE`  — the thread was **already** initialized *in the same mode*. This is NOT
///                  a no-op: the reference count **was** incremented, so it must still be
///                  balanced by a `CoUninitialize`. Treating `S_FALSE` as "nothing to undo"
///                  is the classic leak here, and `HRESULT::is_ok()` is true for it, which
///                  is exactly why the two are handled together below rather than by
///                  matching `S_OK` alone.
///   * `RPC_E_CHANGED_MODE` — the thread is already an MTA and refuses to become an STA.
///                  Our call took **no** reference, so we must **not** release one. (We
///                  always spawn a fresh thread, so this cannot happen in practice; it is
///                  handled because getting it wrong would unbalance somebody else's COM.)
///
/// RAII rather than a call pair: `delete_path` has several early returns, and an early
/// return that skips `CoUninitialize` leaks an apartment reference for the life of the
/// thread. `Drop` cannot be skipped.
#[cfg(windows)]
struct ComApartment {
    /// Did our `CoInitializeEx` take a reference that we owe back?
    owns_reference: bool,
}

#[cfg(windows)]
impl ComApartment {
    fn enter() -> Self {
        use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        // SAFETY: plain COM initialization on the calling thread; no pointers are handed
        // across, and the matching CoUninitialize is guaranteed by Drop below.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            return Self {
                owns_reference: false,
            };
        }
        // S_OK *and* S_FALSE both took a reference. `is_ok()` covers both.
        Self {
            owns_reference: hr.is_ok(),
        }
    }
}

#[cfg(windows)]
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owns_reference {
            use windows::Win32::System::Com::CoUninitialize;
            // SAFETY: balances exactly the CoInitializeEx in `enter`, on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Turn `SHFileOperationW`'s return code into something a human can act on.
///
/// Its codes are its OWN — they are not `GetLastError` values, and they are not `HRESULT`s.
/// The set below is the documented `DE_*` list plus the plain Win32 codes the shell also
/// passes through. Anything unrecognized keeps the raw number in the message: a bug report
/// with a code beats prose that invented a cause.
pub fn describe_delete_failure(rc: i32) -> String {
    match rc {
        0x02 => "the item no longer exists".into(),
        0x05 | 0x78 => "access denied — you may not have permission to delete it".into(),
        0x20 => "the file is open in another program".into(),
        0x75 | 0x4C7 => "the delete was cancelled".into(),
        0x79 | 0x7A => "the path is too long for the shell to handle".into(),
        0x7C => "the path is not valid".into(),
        0x85 => "the disk is full".into(),
        0x10000 => "the Recycle Bin refused the item (it may be too large, or on a drive with no bin)".into(),
        // 0x402 is undocumented and, in the wild, almost always a path the shell couldn't
        // resolve. Say what we can and keep the code.
        0x402 => "the shell could not find the item (code 0x402)".into(),
        other => format!("the shell refused to delete this item (code 0x{other:x})"),
    }
}

#[cfg(windows)]
fn delete_path(path: &Path, _is_dir: bool) -> Result<bool, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
        FO_DELETE, SHFILEOPSTRUCTW,
    };

    // Enter a single-threaded apartment for the duration of the call (#216). On the old
    // main-thread path this was satisfied by wry's OleInitialize; on the worker thread we
    // now run on, it is satisfied here. Held for the whole function: Drop releases it on
    // every exit path, including the early returns below.
    let _com = ComApartment::enter();

    // SHFileOperationW takes a DOUBLE-nul-terminated list of paths. One path here,
    // so: <path> NUL NUL. Getting this wrong reads past the buffer, so it is worth
    // being explicit about. (Moving to a worker thread changed nothing here — the buffer
    // is still built, used and dropped entirely inside this call.)
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.push(0);
    from.push(0);

    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE as u32,
        pFrom: windows::core::PCWSTR(from.as_ptr()),
        // ALLOWUNDO is the Recycle Bin. The other three suppress the shell's OWN UI:
        //   FOF_SILENT        — no shell progress dialog. The PANE shows progress now
        //                       (#216), and a second, separate, shell-owned window
        //                       floating over loomux would be both redundant and a
        //                       focus-stealing surprise.
        //   FOF_NOCONFIRMATION — loomux already asked.
        //   FOF_NOERRORUI      — errors come back as a code we translate and toast, rather
        //                        than as a modal the user has to dismiss.
        // Together these are the "no UI at all" set, which is also why the worker thread
        // needs no message pump: nothing here can create a window.
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    // SAFETY: `from` is a double-nul-terminated UTF-16 buffer that outlives the
    // call, and `op` is fully initialized. COM is initialized on this thread (above).
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 {
        return Err(err("io", describe_delete_failure(rc)));
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

// ---------- reveal in the OS file manager ----------

/// Show `rel` in the operating system's own file manager, **with the entry selected**
/// — Explorer's "Reveal in folder", Finder's "Show in Finder".
///
/// Best-effort by platform, and honestly so — `capabilities()` tells the UI which of
/// these it is getting, and the menu says so rather than promising a selection it
/// cannot deliver:
///
///   * **Windows** — `explorer /select,<path>`. Exact: the entry is selected.
///   * **macOS** — `open -R <path>`. Exact: the entry is selected.
///   * **Linux** — there is no standard "reveal" verb. `xdg-open <parent dir>` opens
///     the containing folder in whatever file manager is configured, with **nothing
///     selected**. That is the honest limit of what is portable; a per-DE special case
///     (`nautilus --select`, `dolphin --select`, …) is a matrix we are not signing up
///     to maintain in a terminal multiplexer.
///
/// A DIRECTORY reveals *itself* (selected in its parent), which is what every file
/// manager does and what the user means by right-clicking a folder → Reveal.
pub fn reveal(root: &str, rel: &str) -> Result<(), String> {
    let path = resolve(root, rel)?;
    if !path.exists() {
        return Err(err("not-found", format!("'{rel}' no longer exists")));
    }
    reveal_os(&path)
}

#[cfg(windows)]
fn reveal_os(path: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};
    // `/select,<path>` must arrive as ONE argv element — Explorer parses everything
    // after the comma as the path, so this is not a shell string being concatenated;
    // it is a single argument built with `format!` and handed straight to CreateProcess.
    // A filename full of spaces, quotes or ampersands is inert (there is no shell).
    let mut cmd = Command::new("explorer.exe");
    cmd.arg(format!("/select,{}", path.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Explorer notoriously returns a NON-ZERO exit code on success, so its status is
    // worthless — only the spawn itself can meaningfully fail.
    cmd.spawn().map(|_| ()).map_err(|e| err("io", e.to_string()))
}

#[cfg(not(windows))]
fn reveal_os(path: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};
    // argv, never a shell string — the same guarantee as `open_os` and `editor.rs`.
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg("-R").arg(path); // -R = reveal: opens the folder AND selects the entry
        c
    } else {
        // No portable reveal on Linux: open the containing folder, select nothing.
        let mut c = Command::new("xdg-open");
        c.arg(path.parent().unwrap_or(path));
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| err("io", format!("could not reveal: {e}")))
}

// ---------- open with… ----------

/// Ask the OS to show its **"Open with"** chooser for `rel`, so the user can send the
/// file to an application other than the default.
///
/// **Windows only.** `ShellExecuteW` with the `openas` verb pops the same chooser
/// Explorer's own "Open with →  Choose another app" does — and, like the default-open,
/// it takes a **path**, never a command line, so there is nothing for a shell to
/// re-parse.
///
/// macOS and Linux get a typed `unsupported` error rather than a bad approximation.
/// There is no clean CLI for the Finder chooser (`open -a <App>` needs you to already
/// know the app, which is precisely what the dialog is for), and Linux has no standard
/// at all. `capabilities()` reports this, and the menu **hides** the item on those
/// platforms rather than offering something that fails when clicked.
pub fn open_with(root: &str, rel: &str) -> Result<(), String> {
    let path = resolve(root, rel)?;
    let md = std::fs::symlink_metadata(&path).map_err(|e| err("not-found", e.to_string()))?;
    if md.is_dir() {
        return Err(err("is-dir", "directories are navigated, not opened"));
    }
    open_with_os(&path)
}

#[cfg(windows)]
fn open_with_os(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // The `openas` verb IS the Open-with dialog. (SHOpenWithDialog would also work but
    // wants an HWND and an OPENASINFO; this needs neither and is the same chooser.)
    let verb: Vec<u16> = "openas\0".encode_utf16().collect();

    // SAFETY: both buffers are nul-terminated and outlive the call; the path is passed
    // as a PATH (lpFile), with lpParameters NULL — no command line is ever formed.
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
    let code = rc.0 as usize;
    if code > 32 {
        return Ok(());
    }
    Err(match code {
        2 | 3 => err("not-found", "the file could not be found"),
        _ => err(
            "io",
            format!("the shell could not show the Open-with dialog (code {code})"),
        ),
    })
}

#[cfg(not(windows))]
fn open_with_os(_path: &Path) -> Result<(), String> {
    Err(err(
        "unsupported",
        "there is no OS \"open with\" chooser on this platform",
    ))
}

// ---------- capabilities ----------

/// What this platform can actually do, so the UI offers exactly that. Reported once and
/// consulted when the context menu is built: an item that would always fail is HIDDEN
/// rather than shown-and-broken, and one that is approximate (Linux reveal) says so.
#[derive(Serialize, Debug, PartialEq)]
pub struct Caps {
    /// "recycle" | "permanent" — what `delete` will do here (see `delete_mode`).
    pub delete_mode: &'static str,
    /// Is there an OS "Open with" chooser? Windows only.
    pub open_with: bool,
    /// Can we reveal in the OS file manager, and does it SELECT the entry?
    pub reveal: bool,
    /// True when reveal opens the containing folder but cannot select the entry
    /// (Linux). The menu labels it honestly instead of over-promising.
    pub reveal_selects: bool,
}

pub fn capabilities() -> Caps {
    Caps {
        delete_mode: delete_mode().mode,
        open_with: cfg!(windows),
        reveal: true,
        reveal_selects: cfg!(any(windows, target_os = "macos")),
    }
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
pub fn fm_new_file(root: String, rel: String, name: String) -> Result<String, String> {
    new_file(&root, &rel, &name)
}

#[tauri::command]
pub fn fm_rename(root: String, rel: String, name: String) -> Result<String, String> {
    rename(&root, &rel, &name)
}

/// One completed delete, streamed back as an `fm-delete` event (#216).
///
/// **The contract the pane reads:** exactly one of `recycled`/`error` is present, and the
/// pane branches on `error`. Both absent would render as a silent success on a delete that
/// never happened, so `delete_event` below is the *only* constructor — see the test that
/// pins the wire shape.
#[derive(Clone, Serialize)]
pub struct DeleteEvent {
    id: u64,
    /// The `rel` the delete was asked to remove — so the pane can match the result to the
    /// row it marked busy, even if it has navigated elsewhere since.
    rel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    recycled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Build the completion event from a delete's outcome. Factored out of the worker (which
/// needs a live `AppHandle` to emit, and so can't be reached from a test) precisely so the
/// **payload contract** can be: success ⇒ `recycled` and no `error`; failure ⇒ `error` and
/// no `recycled`. The frontend's success/failure branch is `error != null`, and this is the
/// one place that could ever break it.
pub fn delete_event(id: u64, rel: String, outcome: Result<bool, String>) -> DeleteEvent {
    match outcome {
        Ok(recycled) => DeleteEvent {
            id,
            rel,
            recycled: Some(recycled),
            error: None,
        },
        Err(e) => DeleteEvent {
            id,
            rel,
            recycled: None,
            error: Some(e),
        },
    }
}

/// Delete `rel` on a **worker thread**, and report the outcome as an `fm-delete` event
/// tagged with the caller's `id` (issue #216).
///
/// ## Why this is not a synchronous command
///
/// Tauri runs a sync command on the **main (webview) thread**. `SHFileOperationW` over a
/// `node_modules`-sized tree is tens of thousands of file operations and takes seconds to
/// minutes — for all of which the entire window, every pane, is frozen. Same class as the
/// #207 search freeze; the same shape of fix.
///
/// ## Why it is not an `async fn` either
///
/// This is the part that made it worth deferring rather than rushing (rev-102, round 3).
/// A plain `async fn` command runs on Tauri's async runtime, on a thread pool whose COM
/// state is nobody's business — and `SHFileOperationW` is a **Shell/COM** API whose
/// apartment requirement the main thread was silently satisfying for us (wry
/// `OleInitialize`s it as an STA). Offloading it naively trades a freeze for an
/// apartment question. So this spawns a **dedicated OS thread** that enters its own STA
/// (`ComApartment`, above) for the duration and leaves it on the way out.
///
/// ## Why there is NO cancellation — deliberately
///
/// The other three streams on the shared id counter (search, name index, hashing) are all
/// cancellable, and this one is conspicuously not. That is a decision, not an oversight:
///
///   * `SHFileOperationW` is **one call**. It exposes no cancel handle, no progress sink,
///     and no way to ask it to stop. (`IFileOperation` does, via
///     `IFileOperationProgressSink` — a different, much larger API surface, and a
///     different conversation.)
///   * Even if it could be stopped, a delete cancelled halfway through a tree leaves a
///     **half-deleted directory** — some children in the Recycle Bin, some not, and a
///     parent that may or may not still exist. That is a worse outcome than waiting.
///
/// So a delete, once confirmed, runs to completion. The UI says *"Deleting…"* and does not
/// offer a Cancel button it could not honor. It does not register a flag in the cancel
/// registry either — a flag nobody polls is a lie told to the next reader.
///
/// Navigation and hashing stay live throughout: they touch nothing the delete owns, and
/// freezing them in the UI would reintroduce, one layer up, exactly the unresponsiveness
/// this change exists to remove.
#[tauri::command]
pub fn fm_delete_start(app: AppHandle, id: u64, root: String, rel: String) {
    std::thread::spawn(move || {
        // The apartment is entered inside `delete_path` (Windows), so the pure `delete`
        // fn is correct on ANY thread — which is also what lets the integration tests
        // drive the real, COM-initialized path rather than a stubbed one.
        let event = delete_event(id, rel.clone(), delete(&root, &rel));
        // A failed emit means the window is gone. The delete already happened; there is
        // nobody left to tell, and that is exactly what closing the pane means.
        let _ = app.emit("fm-delete", event);
    });
}

#[tauri::command]
pub fn fm_capabilities() -> Caps {
    capabilities()
}

#[tauri::command]
pub fn fm_reveal(root: String, rel: String) -> Result<(), String> {
    reveal(&root, &rel)
}

#[tauri::command]
pub fn fm_open_with(root: String, rel: String) -> Result<(), String> {
    open_with(&root, &rel)
}

#[tauri::command]
pub fn fm_open(root: String, rel: String) -> Result<(), String> {
    open_default(&root, &rel)
}
