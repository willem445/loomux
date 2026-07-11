//! In-app file tree + code editor backend (issue #174).
//!
//! Powers the per-pane file-editor overlay: a lazy directory tree, file
//! read/write, and a project-wide text search + replace. Every command takes a
//! `root` (the pane's live cwd) and a `rel` path *relative to that root*; the
//! webview is trusted but paths still cross the IPC boundary, so **all** path
//! safety is enforced here, defense-in-depth (CLAUDE.md constraint #6):
//!
//!   * absolute `rel` is rejected;
//!   * `.`/`..` are folded lexically (never `fs::canonicalize`, which yields a
//!     `\\?\`-verbatim path some Windows toolchains mishandle — same reason
//!     `pty::lexical_normalize` avoids it) and the result must stay inside the
//!     normalized `root`;
//!   * we refuse to traverse, read, or write *through* a symlinked component,
//!     since not canonicalizing means a symlink could otherwise redirect a
//!     lexically-in-root path to somewhere outside it.
//!
//! Writes are atomic + durable (temp in the same dir → `write_all` → `sync_all`
//! fsync → `rename`, with a direct-write fallback), the exact #133/#161 pattern
//! already duplicated in `orchestration::atomic_write` and `uistate::write_atomic`
//! — a third private copy here matches house style. A content hash returned by
//! `ft_read_file` and re-checked by `ft_write_file` gives optimistic
//! conflict detection: if the file changed on disk since it was read (the agent,
//! another tool, or the git watcher touched it), the write is refused untouched.
//!
//! No new crates: the hash is a std-only FNV-1a and temp names use an
//! `AtomicU64` + `process::id()`, never uuid/rand/tempfile-in-prod (the
//! getrandom / ProcessPrng ban, CLAUDE.md constraint #2). Search is a pure-`std`
//! walker — dependency-free, getrandom-safe, and bounded so a huge repo can't
//! wedge the UI.
//!
//! Search runs off the UI thread and streams (issue #207). `ft_search` was a
//! *synchronous* command: Tauri runs sync commands on the main (webview) thread,
//! so a full-tree walk that reads tens of thousands of files froze the whole UI
//! for its duration — and the debounced auto-search relaunched that walk on every
//! keystroke. `ft_search_start` instead spawns a worker thread that walks, emits
//! `ft-search` batches as they're found (tagged with the caller's search id), and
//! polls a per-search cancel flag so a superseded search (new keystroke) or an
//! `Esc` stops promptly instead of running to completion. Enumeration is
//! gitignore-aware by default: in a git repo it uses `git ls-files` (tracked +
//! untracked-unignored) so `.gitignore`d paths are skipped for free; the
//! `include_ignored` toggle (or a non-git root) falls back to the full walk. The
//! git call is a plain subprocess — still no new crate, still getrandom-safe.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::obs::LockExt;
use tauri::{AppHandle, Emitter, State};

/// Files larger than this are refused by `ft_read_file` — the editor is for
/// source, not blobs, and loading multi-megabyte buffers into the webview would
/// stall it. 2 MiB comfortably covers even generated source.
const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;

/// Files larger than this are skipped by the search walker. Smaller than the
/// read cap: search scans *many* files, so the per-file bound is tighter.
const MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;

/// Prefix of the byte window scanned for a NUL to classify a file as binary.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Hard ceiling on matches returned by one search, independent of the caller's
/// `max_results` — a runaway query still can't flood the IPC channel. When hit,
/// the result is flagged `truncated` (never silently cut, CLAUDE.md house rule).
const SEARCH_MATCH_CEILING: usize = 5_000;

/// Hard ceiling on files visited by one search walk, so a giant tree can't hang
/// the walker even with generous excludes.
const SEARCH_FILE_CEILING: usize = 50_000;

/// Matches are streamed to the UI in batches of this size (issue #207) so results
/// appear as they're found and one giant payload never crosses the IPC boundary.
/// The cancel flag is polled between files, so a batch bounds how long a
/// superseded search keeps working before it notices.
const SEARCH_BATCH: usize = 256;

/// Directory names never descended into by the search walker: VCS metadata and
/// the usual heavy build/dependency dirs. Any dot-directory is skipped too (see
/// `is_excluded_dir`) so `.git`, `.venv`, `.next`, … need no explicit listing.
const EXCLUDED_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "vendor"];

/// Monotonic counter feeding unique temp-file names, so concurrent saves in the
/// same directory can't collide. Paired with `process::id()` — no getrandom.
static ATOMIC_WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

// ---------- wire types ----------

/// One entry in a directory listing. `is_symlink` entries are shown but never
/// expanded/followed (see the module-level symlink note); `size` is 0 for dirs
/// and symlinks.
#[derive(Serialize, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
}

/// A file's decoded contents plus the hash the frontend echoes back on save for
/// conflict detection. `truncated` is reserved for a future partial-read mode;
/// today an over-cap file is refused outright, so it is always `false`.
#[derive(Serialize, Debug)]
pub struct FileRead {
    pub content: String,
    pub hash: String,
    pub truncated: bool,
}

/// Result of a successful write: the new on-disk content hash, which the
/// frontend adopts as the baseline for the next conflict check.
#[derive(Serialize, Debug)]
pub struct WriteResult {
    pub hash: String,
}

/// A single search hit. `line`/`col` are 1-based (col counts characters, not
/// bytes, so it lines up with what an editor shows); `line_text` is the matched
/// line, trimmed of the trailing newline and capped for display.
#[derive(Serialize, Debug, Clone)]
pub struct Match {
    pub rel: String,
    pub line: usize,
    pub col: usize,
    pub line_text: String,
}

/// Search output. `truncated` is set when a match/file ceiling cut the walk
/// short, so the UI can say "showing first N" rather than imply completeness.
#[derive(Serialize, Debug)]
pub struct SearchOutcome {
    pub matches: Vec<Match>,
    pub truncated: bool,
}

/// Knobs for a search/replace. `max_results` is clamped to `SEARCH_MATCH_CEILING`.
#[derive(Deserialize, Clone, Copy, Default)]
pub struct SearchOpts {
    #[serde(default)]
    pub case_insensitive: bool,
    #[serde(default)]
    pub whole_word: bool,
    #[serde(default)]
    pub max_results: usize,
    /// Include files git would ignore (issue #207). Default (`false`) enumerates
    /// via `git ls-files` so `.gitignore` is respected; `true` walks the full
    /// tree. No effect on `replace`, which acts on an explicit file list.
    #[serde(default)]
    pub include_ignored: bool,
}

/// A file the replace pass could not touch, with a human-readable reason. One
/// bad file never aborts the batch and never leaves a partial write.
#[derive(Serialize, Debug)]
pub struct SkippedFile {
    pub rel: String,
    pub reason: String,
}

/// Outcome of an apply pass: which files changed and how many matches each got,
/// plus the ones skipped.
#[derive(Serialize, Debug)]
pub struct ReplaceResult {
    pub changed: Vec<ChangedFile>,
    pub skipped: Vec<SkippedFile>,
}

#[derive(Serialize, Debug)]
pub struct ChangedFile {
    pub rel: String,
    pub replacements: usize,
}

// ---------- error codes ----------
//
// Errors are plain strings (house style — see git.rs/editor.rs) but every one
// starts with a stable machine code followed by ": ", so the frontend can
// switch on the code (conflict → offer reload/overwrite, binary/too-large →
// explain why the file won't open) without parsing prose. Keep these in sync
// with the `FileEditError` discriminants in `src/fileapi.ts`.

fn err(code: &str, msg: impl AsRef<str>) -> String {
    format!("{code}: {}", msg.as_ref())
}

// ---------- path safety ----------

/// Resolve `.`/`..` lexically, without touching the filesystem. A private copy
/// of `pty::lexical_normalize` (that one is module-private, and the helper is
/// already duplicated per module in this codebase — house style). Verbatim/UNC
/// prefixes and the root are preserved; only `.`/`..` are folded.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = Vec::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                // Only pop a *normal* segment; never climb above a prefix/root.
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out.iter().collect()
}

/// Turn a `(root, rel)` pair into a validated absolute path inside `root`, or a
/// typed error. `rel` may be empty (means `root` itself). This is the single
/// choke point every command routes through.
fn safe_resolve(root: &str, rel: &str) -> Result<PathBuf, String> {
    let root_path = Path::new(root);
    if !root_path.is_dir() {
        return Err(err("not-found", format!("root is not a directory: {root}")));
    }
    let root_norm = lexical_normalize(root_path);

    let rel_path = Path::new(rel);
    if rel_path.is_absolute() || rel_path.has_root() {
        return Err(err("invalid-path", format!("path must be relative: {rel}")));
    }
    // A Windows drive-relative segment (`C:foo`) or any explicit prefix in `rel`
    // would let it escape; reject anything that isn't a plain segment.
    if rel_path
        .components()
        .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
    {
        return Err(err("invalid-path", format!("path must be relative: {rel}")));
    }

    let joined = lexical_normalize(&root_norm.join(rel_path));
    if !joined.starts_with(&root_norm) {
        return Err(err("outside-root", format!("path escapes the root: {rel}")));
    }
    ensure_no_symlink(&root_norm, &joined)?;
    Ok(joined)
}

/// Refuse if any component of `target` *below* `root` is a symlink. Called after
/// the lexical within-root check: without canonicalization, a symlinked segment
/// is the one remaining way a lexically-in-root path could redirect outside.
/// Non-existent tail components (e.g. a brand-new file being written) simply
/// have no metadata and are treated as non-symlinks — their parents were still
/// checked.
fn ensure_no_symlink(root: &Path, target: &Path) -> Result<(), String> {
    let rest = target
        .strip_prefix(root)
        .map_err(|_| err("outside-root", "path escapes the root"))?;
    let mut cur = root.to_path_buf();
    for comp in rest.components() {
        cur.push(comp);
        if let Ok(md) = std::fs::symlink_metadata(&cur) {
            if md.file_type().is_symlink() {
                return Err(err(
                    "symlink",
                    format!("refusing to traverse symlink: {}", cur.display()),
                ));
            }
        }
    }
    Ok(())
}

/// `root`-relative, forward-slashed display path for `abs` (already validated to
/// be under `root`). Forward slashes keep the frontend grouping/display stable
/// across platforms.
fn rel_display(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root)
        .unwrap_or(abs)
        .to_string_lossy()
        .replace('\\', "/")
}

// ---------- hashing ----------

/// FNV-1a 64-bit over the raw bytes, hex-encoded. Not cryptographic — it only
/// has to detect that a file changed since it was read, and it must avoid any
/// getrandom-seeded hasher (CLAUDE.md constraint #2). Deterministic, so tests
/// can assert exact values.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

// ---------- atomic durable write ----------

/// Atomically write `bytes` to `path`: unique sibling temp → `write_all` →
/// `sync_all` fsync (the disk-full guard #133 added) → `rename` over the target,
/// with a direct-write fallback if the rename is briefly blocked (a scanner or
/// another handle on Windows). A crash leaves either the old file or the temp,
/// never a truncated target. `path`'s parent must already exist (it does — it's
/// a directory inside the validated root).
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let seq = ATOMIC_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{stem}.{}.{seq}.tmp", std::process::id()));
    // Write + fsync the temp; on any failure remove the partial sibling so a
    // write/fsync error (e.g. disk full) can't leave an orphan `.tmp` behind.
    let written = (|| -> Result<(), String> {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).map_err(|e| err("io", e.to_string()))?;
        f.write_all(bytes).map_err(|e| err("io", e.to_string()))?;
        f.sync_all().map_err(|e| err("io", e.to_string()))?; // durable before rename
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if std::fs::rename(&tmp, path).is_err() {
        let direct = std::fs::write(path, bytes);
        let _ = std::fs::remove_file(&tmp);
        direct.map_err(|e| err("io", e.to_string()))?;
    }
    Ok(())
}

// ---------- text matching (shared by search + replace) ----------

fn is_word_byte(b: u8) -> bool {
    // Treat any non-ASCII byte as a word byte: it's part of a multibyte UTF-8
    // char, so a whole-word boundary must not fall inside it.
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Byte offsets of every non-overlapping match of `needle` in `hay`. Literal
/// substring search; `ci` folds ASCII case only (non-ASCII is compared exactly)
/// which keeps every match aligned on a UTF-8 boundary — essential so the
/// replace pass can splice bytes without corrupting multibyte characters.
fn match_positions(hay: &[u8], needle: &[u8], ci: bool, whole_word: bool) -> Vec<usize> {
    let mut out = Vec::new();
    let n = needle.len();
    if n == 0 || n > hay.len() {
        return out;
    }
    let mut i = 0;
    while i + n <= hay.len() {
        let cand = &hay[i..i + n];
        let hit = if ci {
            cand.eq_ignore_ascii_case(needle)
        } else {
            cand == needle
        };
        if hit {
            let left_ok = i == 0 || !is_word_byte(hay[i - 1]);
            let right_ok = i + n == hay.len() || !is_word_byte(hay[i + n]);
            if !whole_word || (left_ok && right_ok) {
                out.push(i);
                i += n;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Replace every match of `query` in `text` with `replacement`, returning the
/// new text and the count. Operates on bytes (matches are UTF-8-aligned, above)
/// so the result is always valid UTF-8.
fn replace_all(text: &str, query: &str, replacement: &str, opts: SearchOpts) -> (String, usize) {
    let hay = text.as_bytes();
    let positions = match_positions(hay, query.as_bytes(), opts.case_insensitive, opts.whole_word);
    if positions.is_empty() {
        return (text.to_string(), 0);
    }
    let n = query.len();
    let mut out: Vec<u8> = Vec::with_capacity(hay.len());
    let mut last = 0;
    for &p in &positions {
        out.extend_from_slice(&hay[last..p]);
        out.extend_from_slice(replacement.as_bytes());
        last = p + n;
    }
    out.extend_from_slice(&hay[last..]);
    // Matches are UTF-8-aligned and `replacement` is valid UTF-8, so this can't
    // fail; fall back to the original on the impossible chance it does.
    (
        String::from_utf8(out).unwrap_or_else(|_| text.to_string()),
        positions.len(),
    )
}

/// A byte slice is treated as text if its first `BINARY_SNIFF_BYTES` contain no
/// NUL. Cheap, and matches how git/ripgrep decide binary-ness.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .take(BINARY_SNIFF_BYTES)
        .any(|&b| b == 0)
}

// ---------- read side ----------

/// List one directory (lazy expand — never a full-tree walk). Entries are sorted
/// dirs-first then case-insensitively by name, so the tree is stable without the
/// frontend having to re-sort (it does anyway, for its own merges).
pub fn list_dir(root: &str, rel: &str) -> Result<Vec<Entry>, String> {
    let dir = safe_resolve(root, rel)?;
    let md = std::fs::symlink_metadata(&dir).map_err(|e| err("not-found", e.to_string()))?;
    if !md.is_dir() {
        return Err(err("not-dir", format!("not a directory: {rel}")));
    }
    let mut entries = Vec::new();
    for ent in std::fs::read_dir(&dir).map_err(|e| err("io", e.to_string()))? {
        let ent = match ent {
            Ok(e) => e,
            Err(_) => continue, // skip an unreadable entry rather than fail the listing
        };
        let name = ent.file_name().to_string_lossy().into_owned();
        // Own-type metadata: a symlink reports as a symlink here (not its target),
        // which is exactly what we want — we list it but won't follow it.
        let ft = match ent.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let is_symlink = ft.is_symlink();
        let (is_dir, size) = if is_symlink {
            (false, 0) // shown, never expanded
        } else if ft.is_dir() {
            (true, 0)
        } else {
            let size = ent.metadata().map(|m| m.len()).unwrap_or(0);
            (false, size)
        };
        entries.push(Entry {
            name,
            is_dir,
            is_symlink,
            size,
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Read a UTF-8 text file under `root`. Refuses binary (NUL in the sniff window
/// or invalid UTF-8) and over-cap files with typed errors, and returns a content
/// hash for later conflict detection.
pub fn read_file(root: &str, rel: &str) -> Result<FileRead, String> {
    let path = safe_resolve(root, rel)?;
    let md = std::fs::symlink_metadata(&path).map_err(|e| err("not-found", e.to_string()))?;
    if md.is_dir() {
        return Err(err("is-dir", format!("path is a directory: {rel}")));
    }
    if md.len() > MAX_READ_BYTES {
        return Err(err(
            "too-large",
            format!("file is {} bytes (limit {MAX_READ_BYTES})", md.len()),
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| err("io", e.to_string()))?;
    if looks_binary(&bytes) {
        return Err(err("binary", "file appears to be binary"));
    }
    let hash = content_hash(&bytes);
    // Exact round-trip matters (a lossy decode would corrupt on save), so require
    // valid UTF-8 rather than `from_utf8_lossy`; non-UTF-8 is "binary" to us.
    let content = String::from_utf8(bytes).map_err(|_| err("binary", "file is not valid UTF-8"))?;
    Ok(FileRead {
        content,
        hash,
        truncated: false,
    })
}

// ---------- write side ----------

/// Write `content` to `rel` atomically. If `expected_hash` is provided it must
/// match the file's current on-disk hash or the write is refused with a
/// `conflict` error and the file is left byte-for-byte untouched — the optimistic
/// concurrency guard for "someone else edited this while it was open". Pass
/// `None` (or an empty string) only when creating a brand-new file.
pub fn write_file(
    root: &str,
    rel: &str,
    content: &str,
    expected_hash: Option<String>,
) -> Result<WriteResult, String> {
    let path = safe_resolve(root, rel)?;
    if path.is_dir() {
        return Err(err("is-dir", format!("path is a directory: {rel}")));
    }
    if let Some(expected) = expected_hash.filter(|h| !h.is_empty()) {
        // The caller read the file first and expects it unchanged. Compare
        // against what's on disk *now*; any drift (edited, deleted) is a conflict.
        match std::fs::read(&path) {
            Ok(cur) => {
                let disk = content_hash(&cur);
                if disk != expected {
                    return Err(err(
                        "conflict",
                        "file changed on disk since it was opened",
                    ));
                }
            }
            Err(_) => {
                return Err(err("conflict", "file no longer exists on disk"));
            }
        }
    }
    atomic_write(&path, content.as_bytes())?;
    Ok(WriteResult {
        hash: content_hash(content.as_bytes()),
    })
}

// ---------- search side ----------

fn is_excluded_dir(name: &str) -> bool {
    name.starts_with('.') || EXCLUDED_DIRS.contains(&name)
}

/// Whether the walk should descend into a directory named `name`. `.git` is
/// never searched (VCS metadata, not source) regardless of mode. When
/// `apply_excludes` is set the heuristic ignore list (dot-dirs + the common
/// heavy build/dependency dirs) is applied too — that's the best-effort ignore
/// for a non-git root; a git root filters via `git ls-files` instead, and the
/// `include_ignored` toggle drops the heuristic so even `node_modules` is walked.
fn should_skip_dir(name: &str, apply_excludes: bool) -> bool {
    name == ".git" || (apply_excludes && is_excluded_dir(name))
}

/// How a search discovers candidate files.
enum Enumeration {
    /// Recursively walk the tree; `apply_excludes` toggles the heuristic ignore
    /// list (see `should_skip_dir`).
    Walk { apply_excludes: bool },
    /// An explicit, already root-relative file list (from `git ls-files`).
    Files(Vec<String>),
}

/// Run `git` in `dir` and capture stdout, or `None` on any failure (git missing,
/// not a repo, non-zero exit). A local, Option-returning twin of `git::run_git`
/// — the file editor keeps its own tiny helper (house style duplicates per
/// module) so a search never fails just because git is unavailable; it falls
/// back to the walk. Never spawns a console window on Windows.
fn git_output(dir: &str, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// The gitignore-respecting file set for `root`: tracked files plus
/// untracked-but-not-ignored ones (`--others --exclude-standard`), so a newly
/// created, not-yet-committed source file is searchable while a `.gitignore`d
/// path is not. `None` when `root` isn't a git work tree (or git is missing), so
/// the caller walks instead. Paths are NUL-delimited (`-z`) and forward-slashed
/// (git's native form) — exactly the `rel` convention the rest of this module
/// uses, no re-encoding needed.
fn git_tracked_files(root: &str) -> Option<Vec<String>> {
    // Confirm it's really a work tree first: a stray or corrupt `.git` must fall
    // through to the walk, not surface a git error as a failed search.
    if git_output(root, &["rev-parse", "--is-inside-work-tree"])?.trim() != "true" {
        return None;
    }
    let out = git_output(
        root,
        &["ls-files", "-z", "--cached", "--others", "--exclude-standard"],
    )?;
    Some(
        out.split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Pick the enumeration strategy: gitignore-aware `git ls-files` by default in a
/// git repo; the full walk when `include_ignored` is set or the root isn't a git
/// work tree (`apply_excludes` then keeps the heuristic ignore for a non-git
/// root, and is dropped when the user explicitly asked to include ignored files).
fn plan_enumeration(root: &str, include_ignored: bool) -> Enumeration {
    if !include_ignored {
        if let Some(files) = git_tracked_files(root) {
            return Enumeration::Files(files);
        }
    }
    Enumeration::Walk {
        apply_excludes: !include_ignored,
    }
}

/// Scan one file for up to `remaining` matches of `needle`. Returns the matches
/// and whether it stopped because it hit `remaining` (the global-cap signal).
/// Unreadable, over-cap, binary, or non-UTF-8 files yield no matches — the walk
/// skips them exactly as before.
fn scan_file_matches(
    path: &Path,
    rel: &str,
    needle: &[u8],
    opts: SearchOpts,
    remaining: usize,
) -> (Vec<Match>, bool) {
    let mut out = Vec::new();
    if remaining == 0 {
        return (out, true);
    }
    match std::fs::metadata(path) {
        Ok(m) if m.len() > MAX_SEARCH_FILE_BYTES => return (out, false),
        Ok(_) => {}
        Err(_) => return (out, false),
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return (out, false),
    };
    if looks_binary(&bytes) {
        return (out, false);
    }
    let text = match std::str::from_utf8(&bytes) {
        Ok(t) => t,
        Err(_) => return (out, false),
    };
    let mut hit_cap = false;
    'lines: for (idx, line) in text.lines().enumerate() {
        for &pos in &match_positions(line.as_bytes(), needle, opts.case_insensitive, opts.whole_word) {
            // Byte offset → 1-based character column.
            let col = line[..pos].chars().count() + 1;
            out.push(Match {
                rel: rel.to_string(),
                line: idx + 1,
                col,
                line_text: cap_line(line),
            });
            if out.len() >= remaining {
                hit_cap = true;
                break 'lines;
            }
        }
    }
    (out, hit_cap)
}

/// Buffers matches and flushes them to `on_batch` in `SEARCH_BATCH`-sized chunks,
/// enforcing the global match cap. Keeps the borrow simple so both enumeration
/// arms can share the per-file feed without fighting the borrow checker.
struct Sink<'a> {
    buf: Vec<Match>,
    total: usize,
    cap: usize,
    truncated: bool,
    on_batch: &'a mut dyn FnMut(Vec<Match>),
}

impl Sink<'_> {
    /// Feed one file's worth of scanning. Returns true when the global cap is
    /// reached and the caller should stop the walk.
    fn feed(&mut self, path: &Path, rel: &str, needle: &[u8], opts: SearchOpts) -> bool {
        let remaining = self.cap - self.total;
        let (mut found, hit_cap) = scan_file_matches(path, rel, needle, opts, remaining);
        self.total += found.len();
        self.buf.append(&mut found);
        if self.buf.len() >= SEARCH_BATCH {
            (self.on_batch)(std::mem::take(&mut self.buf));
        }
        if hit_cap {
            self.truncated = true;
        }
        hit_cap
    }

    /// Flush the tail batch and report whether the cap truncated the results.
    fn finish(mut self) -> bool {
        if !self.buf.is_empty() {
            (self.on_batch)(std::mem::take(&mut self.buf));
        }
        self.truncated
    }
}

/// The shared search engine (issue #207). Enumerates per `enumeration`, scans
/// each file, and streams matches to `on_batch` in batches. `cancelled` is polled
/// between files so a superseded/aborted search stops promptly. Returns whether
/// the results were truncated (match or file ceiling hit). Callers: `search`
/// (walk-only collector, used by the tests) and `search_planned` (git-aware,
/// used by the streaming command).
fn run_search(
    root: &str,
    query: &str,
    opts: SearchOpts,
    enumeration: Enumeration,
    cancelled: &dyn Fn() -> bool,
    on_batch: &mut dyn FnMut(Vec<Match>),
) -> Result<bool, String> {
    if query.is_empty() {
        return Err(err("empty-query", "search query is empty"));
    }
    let root_norm = safe_resolve(root, "")?;
    let cap = if opts.max_results == 0 {
        SEARCH_MATCH_CEILING
    } else {
        opts.max_results.min(SEARCH_MATCH_CEILING)
    };
    let needle = query.as_bytes();

    let mut sink = Sink {
        buf: Vec::new(),
        total: 0,
        cap,
        truncated: false,
        on_batch,
    };
    let mut files_seen = 0usize;
    let mut ceiling_hit = false;

    match enumeration {
        Enumeration::Files(files) => {
            for rel in files {
                if cancelled() {
                    break;
                }
                files_seen += 1;
                if files_seen > SEARCH_FILE_CEILING {
                    ceiling_hit = true;
                    break;
                }
                // Re-validate every git-supplied path through the same choke point
                // (rejects anything escaping the root or crossing a symlink).
                let path = match safe_resolve(root, &rel) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                if sink.feed(&path, &rel, needle, opts) {
                    break;
                }
            }
        }
        Enumeration::Walk { apply_excludes } => {
            // Explicit stack instead of recursion: unbounded repo depth mustn't
            // blow the Rust stack, and the ceilings become a simple early-out.
            let mut stack = vec![root_norm.clone()];
            'walk: while let Some(dir) = stack.pop() {
                if cancelled() {
                    break;
                }
                let rd = match std::fs::read_dir(&dir) {
                    Ok(rd) => rd,
                    Err(_) => continue, // unreadable dir: skip, don't fail the search
                };
                for ent in rd.flatten() {
                    let ft = match ent.file_type() {
                        Ok(ft) => ft,
                        Err(_) => continue,
                    };
                    if ft.is_symlink() {
                        continue; // never follow symlinks (dir or file)
                    }
                    let path = ent.path();
                    let name = ent.file_name().to_string_lossy().into_owned();
                    if ft.is_dir() {
                        if !should_skip_dir(&name, apply_excludes) {
                            stack.push(path);
                        }
                        continue;
                    }
                    if !ft.is_file() {
                        continue;
                    }
                    if cancelled() {
                        break 'walk;
                    }
                    files_seen += 1;
                    if files_seen > SEARCH_FILE_CEILING {
                        ceiling_hit = true;
                        break 'walk;
                    }
                    let rel = rel_display(&root_norm, &path);
                    if sink.feed(&path, &rel, needle, opts) {
                        break 'walk;
                    }
                }
            }
        }
    }

    Ok(sink.finish() || ceiling_hit)
}

/// Walk-only literal search that collects every match (issue #174 behaviour).
/// Git-independent and deterministic — the integration tests drive this — so it
/// always applies the heuristic excludes and never consults git. The live,
/// gitignore-aware, streaming path is `search_planned`.
pub fn search(root: &str, query: &str, opts: SearchOpts) -> Result<SearchOutcome, String> {
    let mut matches = Vec::new();
    let truncated = run_search(
        root,
        query,
        opts,
        Enumeration::Walk { apply_excludes: true },
        &|| false,
        &mut |batch| matches.extend(batch),
    )?;
    Ok(SearchOutcome { matches, truncated })
}

/// Streaming, gitignore-aware search (issue #207): picks the enumeration source
/// from `opts.include_ignored` (git ls-files by default, full walk when the
/// toggle is on or the root isn't a git repo) and feeds `on_batch` as matches are
/// found, polling `cancelled` between files. Returns whether results were
/// truncated. Testable without a Tauri runtime; the `ft_search_start` command
/// wires `on_batch` to event emission and `cancelled` to a per-search flag.
pub fn search_planned(
    root: &str,
    query: &str,
    opts: SearchOpts,
    cancelled: &dyn Fn() -> bool,
    on_batch: &mut dyn FnMut(Vec<Match>),
) -> Result<bool, String> {
    let enumeration = plan_enumeration(root, opts.include_ignored);
    run_search(root, query, opts, enumeration, cancelled, on_batch)
}

/// Cap a display line so one very long line can't bloat a result payload.
fn cap_line(s: &str) -> String {
    const MAX: usize = 400;
    if s.len() <= MAX {
        s.to_string()
    } else {
        let mut end = MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

/// Apply `query`→`replacement` to exactly the `files` the caller confirmed (the
/// UI previews via `search` first — no blind whole-tree replace). Each file is
/// re-read and re-matched at apply time and written atomically; a file that
/// can't be read, has no matches, or fails validation is recorded in `skipped`
/// and never partially written. `files` are `root`-relative paths.
pub fn replace(
    root: &str,
    query: &str,
    replacement: &str,
    files: Vec<String>,
    opts: SearchOpts,
) -> Result<ReplaceResult, String> {
    if query.is_empty() {
        return Err(err("empty-query", "search query is empty"));
    }
    let mut changed = Vec::new();
    let mut skipped = Vec::new();
    for rel in files {
        let path = match safe_resolve(root, &rel) {
            Ok(p) => p,
            Err(e) => {
                skipped.push(SkippedFile { rel, reason: e });
                continue;
            }
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                skipped.push(SkippedFile {
                    rel,
                    reason: err("io", e.to_string()),
                });
                continue;
            }
        };
        if looks_binary(&bytes) {
            skipped.push(SkippedFile {
                rel,
                reason: err("binary", "file appears to be binary"),
            });
            continue;
        }
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => {
                skipped.push(SkippedFile {
                    rel,
                    reason: err("binary", "file is not valid UTF-8"),
                });
                continue;
            }
        };
        let (new_text, count) = replace_all(&text, query, replacement, opts);
        if count == 0 {
            // Re-matched to nothing at apply time (file changed since preview):
            // record it so the UI can show it wasn't touched.
            skipped.push(SkippedFile {
                rel,
                reason: err("no-match", "no matches at apply time"),
            });
            continue;
        }
        match atomic_write(&path, new_text.as_bytes()) {
            Ok(()) => changed.push(ChangedFile {
                rel,
                replacements: count,
            }),
            Err(e) => skipped.push(SkippedFile { rel, reason: e }),
        }
    }
    Ok(ReplaceResult { changed, skipped })
}

// ---------- tauri commands ----------
//
// Thin wrappers: all logic lives in the `pub fn`s above so the integration test
// (`tests/fileedit.rs`) can exercise it without a Tauri runtime.

#[tauri::command]
pub fn ft_list_dir(root: String, rel: String) -> Result<Vec<Entry>, String> {
    list_dir(&root, &rel)
}

#[tauri::command]
pub fn ft_read_file(root: String, rel: String) -> Result<FileRead, String> {
    read_file(&root, &rel)
}

#[tauri::command]
pub fn ft_write_file(
    root: String,
    rel: String,
    content: String,
    expected_hash: Option<String>,
) -> Result<WriteResult, String> {
    write_file(&root, &rel, &content, expected_hash)
}

// ---------- streaming search (issue #207) ----------

/// Per-search cancel flags, keyed by the frontend-issued search id. A search
/// worker holds an `Arc` to its own flag and polls it between files; the
/// `ft_search_cancel` command flips it when a newer keystroke (or `Esc`)
/// supersedes the search. Tauri-managed state, shared via `Arc` with each worker
/// thread — the same pattern as `GitWatcher`.
#[derive(Default)]
pub struct SearchRegistry {
    flags: Mutex<HashMap<u64, Arc<AtomicBool>>>,
}

impl SearchRegistry {
    /// Register search `id` and hand back its (freshly un-set) cancel flag.
    fn begin(&self, id: u64) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.flags.lock_safe().insert(id, flag.clone());
        flag
    }

    /// Signal search `id` to stop. No-op if it already finished (its flag is
    /// gone), which is fine — the result is discarded by the id-mismatch guard.
    fn cancel(&self, id: u64) {
        if let Some(flag) = self.flags.lock_safe().get(&id) {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Drop search `id`'s flag once its worker finishes.
    fn end(&self, id: u64) {
        self.flags.lock_safe().remove(&id);
    }
}

/// One streamed batch of a search, tagged with the caller's `id` so the frontend
/// drops events from a superseded/cancelled search. `done` marks the terminal
/// event (carrying final `truncated` + any `error`); `matches` is empty on it.
#[derive(Clone, Serialize)]
struct SearchEvent {
    id: u64,
    matches: Vec<Match>,
    done: bool,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Kick off a streaming search on a worker thread (issue #207). Returns
/// immediately; results arrive as `ft-search` events tagged with `id`. Runs off
/// the UI thread so a big-repo walk never freezes the webview, and polls the
/// per-`id` cancel flag so a superseded search stops promptly.
#[tauri::command]
pub fn ft_search_start(
    app: AppHandle,
    registry: State<'_, Arc<SearchRegistry>>,
    id: u64,
    root: String,
    query: String,
    opts: SearchOpts,
) {
    let flag = registry.begin(id);
    let reg = registry.inner().clone();
    std::thread::spawn(move || {
        let cancelled = || flag.load(Ordering::Relaxed);
        let mut emit = |batch: Vec<Match>| {
            let _ = app.emit(
                "ft-search",
                SearchEvent {
                    id,
                    matches: batch,
                    done: false,
                    truncated: false,
                    error: None,
                },
            );
        };
        let (truncated, error) = match search_planned(&root, &query, opts, &cancelled, &mut emit) {
            Ok(truncated) => (truncated, None),
            Err(e) => (false, Some(e)),
        };
        // Always send a terminal event (even when cancelled): the frontend keys
        // off `id`, so a done for a superseded search is simply ignored.
        let _ = app.emit(
            "ft-search",
            SearchEvent {
                id,
                matches: Vec::new(),
                done: true,
                truncated,
                error,
            },
        );
        reg.end(id);
    });
}

/// Cancel the in-flight search `id` (a new keystroke or `Esc`). Idempotent.
#[tauri::command]
pub fn ft_search_cancel(registry: State<'_, Arc<SearchRegistry>>, id: u64) {
    registry.cancel(id);
}

#[tauri::command]
pub fn ft_replace(
    root: String,
    query: String,
    replacement: String,
    files: Vec<String>,
    opts: SearchOpts,
) -> Result<ReplaceResult, String> {
    replace(&root, &query, &replacement, files, opts)
}
