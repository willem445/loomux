//! External-change watcher for the per-pane git view (issue #36).
//!
//! All git UI refresh is otherwise event-driven off the pane's own shell
//! prompt (OSC 7 → `pane.ts onCwdReported` → branch-chip refresh +
//! `GitView.notifyPrompt`). A `git checkout` / commit / stage run from VS Code
//! or another terminal never touches the pane's shell, so nothing fires and the
//! view goes stale. This module closes that gap.
//!
//! A single background thread polls the `.git` metadata of every repo that has
//! an open pane and emits `git-changed { id }` when it moves. The frontend
//! feeds that into the *same* throttled refresh path as a prompt, so rate
//! limiting and rendering are unchanged — we only add a new trigger.
//!
//! Why polling and not the `notify` crate: this project's Windows 10 baseline
//! can't load binaries that import `bcryptprimitives.dll!ProcessPrng`, so any
//! dependency pulling `getrandom`/`rand` is off-limits (see the note in
//! Cargo.toml). Stat-ing a handful of small files once a second is cheap and
//! pulls in nothing new. The signature also folds in `HEAD`'s *contents* (a
//! ~41-byte file) so a branch switch is detected even where filesystem mtime
//! resolution is coarse.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

/// How often the `.git` metadata of watched repos is sampled. One second keeps
/// the worst-case latency (poll interval + the frontend's 500 ms throttle)
/// comfortably under the ~2 s target while a stat sweep stays negligible.
const POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// One watched repository, keyed in the registry by the owning pane's pty id.
struct Watch {
    /// Worktree-local git dir (holds this checkout's `HEAD` and `index`).
    git_dir: PathBuf,
    /// Shared common dir (holds `refs/`, `packed-refs`, `logs/`); equals
    /// `git_dir` for a normal, non-worktree repo.
    common_dir: PathBuf,
    /// Signature at the last poll; a change means the view should refresh.
    last_sig: u64,
}

/// Registry of per-pane repo watches plus the poll logic. Tauri-managed state;
/// the background thread borrows it through the shared `Arc`.
#[derive(Default)]
pub struct GitWatcher {
    watches: Mutex<HashMap<u32, Watch>>,
}

impl GitWatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Point pane `id` at the repository containing `cwd`, or drop its watch if
    /// `cwd` is not inside a repo. Idempotent and cheap to call on every prompt:
    /// repointing at the same git dir keeps the stored signature, so no spurious
    /// refresh fires and a change that happened mid-interval is still caught on
    /// the next poll.
    pub fn watch(&self, id: u32, cwd: &str) {
        let resolved = resolve_git_dirs(Path::new(cwd));
        let mut map = self.watches.lock().unwrap();
        match resolved {
            Some((git_dir, common_dir)) => {
                let same = map.get(&id).is_some_and(|w| w.git_dir == git_dir);
                if !same {
                    let last_sig = repo_signature(&git_dir, &common_dir);
                    map.insert(
                        id,
                        Watch {
                            git_dir,
                            common_dir,
                            last_sig,
                        },
                    );
                }
            }
            None => {
                map.remove(&id);
            }
        }
    }

    /// Stop watching pane `id` (called when its pane is disposed).
    pub fn unwatch(&self, id: u32) {
        self.watches.lock().unwrap().remove(&id);
    }

    /// Recompute every watch's signature and return the pane ids whose repo
    /// metadata moved since the last poll, updating the stored signatures.
    pub fn poll_changed(&self) -> Vec<u32> {
        let mut changed = Vec::new();
        let mut map = self.watches.lock().unwrap();
        for (id, w) in map.iter_mut() {
            let sig = repo_signature(&w.git_dir, &w.common_dir);
            if sig != w.last_sig {
                w.last_sig = sig;
                changed.push(*id);
            }
        }
        changed
    }

    /// Number of active watches (test/introspection helper).
    #[cfg(test)]
    fn len(&self) -> usize {
        self.watches.lock().unwrap().len()
    }
}

/// Spawn the poll thread. Call once at startup; it runs for the app's life and
/// only touches the filesystem for repos with a live pane.
pub fn start(app: AppHandle, watcher: Arc<GitWatcher>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(POLL_INTERVAL);
        for id in watcher.poll_changed() {
            let _ = app.emit("git-changed", ChangedPayload { id });
        }
    });
}

#[derive(Clone, Serialize)]
struct ChangedPayload {
    id: u32,
}

#[tauri::command]
pub fn git_watch(watcher: State<Arc<GitWatcher>>, id: u32, cwd: String) {
    watcher.watch(id, &cwd);
}

#[tauri::command]
pub fn git_unwatch(watcher: State<Arc<GitWatcher>>, id: u32) {
    watcher.unwatch(id);
}

// ---------- signature ----------

/// A cheap fingerprint of a repo's ref/index state. It changes when `HEAD`
/// moves (checkout), the index is rewritten (stage/commit/checkout), or any ref
/// is created, deleted, or updated (commit/branch/fetch/reset). Computed from
/// `stat` metadata plus `HEAD`'s tiny contents — no `git` subprocess, no full
/// directory reads beyond the small `refs/` tree.
fn repo_signature(git_dir: &Path, common_dir: &Path) -> u64 {
    let mut acc: u64 = 0;
    // HEAD by content: guarantees a branch switch is seen even when two branch
    // names collide in length and the clock is too coarse to move mtime.
    mix_head_contents(&mut acc, &git_dir.join("HEAD"));
    // The rest by stat — size/mtime move whenever these are rewritten.
    mix_stat(&mut acc, &git_dir.join("index"));
    mix_stat(&mut acc, &git_dir.join("logs").join("HEAD"));
    mix_stat(&mut acc, &common_dir.join("packed-refs"));
    mix_stat(&mut acc, &common_dir.join("logs").join("HEAD"));
    mix_tree(&mut acc, &common_dir.join("refs"));
    acc
}

/// Fold a file's `stat` (path, mtime, length) into the accumulator. A missing
/// file contributes nothing, so its creation or deletion changes the sum.
fn mix_stat(acc: &mut u64, path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut h = DefaultHasher::new();
        path.hash(&mut h);
        mtime.hash(&mut h);
        meta.len().hash(&mut h);
        *acc = acc.wrapping_add(h.finish());
    }
}

/// Fold a small file's trimmed contents (keyed by path) into the accumulator.
fn mix_head_contents(acc: &mut u64, path: &Path) {
    if let Ok(content) = std::fs::read_to_string(path) {
        let mut h = DefaultHasher::new();
        path.hash(&mut h);
        content.trim().hash(&mut h);
        *acc = acc.wrapping_add(h.finish());
    }
}

/// Recursively fold every file under `dir` (the loose-ref tree) by `stat`. The
/// tree is small — packed refs live in `packed-refs`, not here.
fn mix_tree(acc: &mut u64, dir: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => mix_tree(acc, &entry.path()),
            Ok(_) => mix_stat(acc, &entry.path()),
            Err(_) => {}
        }
    }
}

// ---------- .git resolution ----------

/// Walk up from `cwd` to the enclosing repo and return `(git_dir, common_dir)`,
/// or None when `cwd` is not inside a git repository. Mirrors the `.git`
/// resolution in `pty.rs` but additionally follows the worktree `commondir`.
fn resolve_git_dirs(cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut cur = Some(cwd);
    while let Some(d) = cur {
        if let Some(git_dir) = resolve_dot_git(&d.join(".git")) {
            let common_dir = resolve_common_dir(&git_dir);
            return Some((git_dir, common_dir));
        }
        cur = d.parent();
    }
    None
}

/// Resolve a `.git` entry to its git dir. It is either a directory (normal
/// repo) or a `gitdir: <path>` pointer file (worktrees and submodules).
fn resolve_dot_git(dot_git: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git.to_path_buf());
    }
    let pointer = std::fs::read_to_string(dot_git).ok()?;
    let rel = pointer.trim().strip_prefix("gitdir:")?.trim();
    let path = Path::new(rel);
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        Some(dot_git.parent()?.join(path))
    }
}

/// The shared common dir for a git dir. A linked worktree's git dir carries a
/// `commondir` file pointing at the main `.git`; a normal repo has none, so its
/// git dir is its own common dir.
fn resolve_common_dir(git_dir: &Path) -> PathBuf {
    if let Ok(rel) = std::fs::read_to_string(git_dir.join("commondir")) {
        let rel = rel.trim();
        let path = Path::new(rel);
        return if path.is_absolute() {
            path.to_path_buf()
        } else {
            git_dir.join(path)
        };
    }
    git_dir.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// A path whose ancestors provably contain no `.git`. The OS temp dir is
    /// *not* a reliable "outside a repo" location — home directories are often
    /// git repos themselves (dotfiles), and resolution walks up to the root —
    /// so the negative cases use a nonexistent top-level location instead.
    fn no_repo_path() -> PathBuf {
        #[cfg(windows)]
        {
            PathBuf::from(r"Q:\loomux-no-such-repo\a\b")
        }
        #[cfg(not(windows))]
        {
            PathBuf::from("/loomux-no-such-repo-xyz/a/b")
        }
    }

    /// Build a minimal but realistic loose-ref repo layout under `root/.git`.
    fn init_git(root: &Path) {
        let git = root.join(".git");
        fs::create_dir_all(git.join("refs").join("heads")).unwrap();
        fs::create_dir_all(git.join("logs")).unwrap();
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(git.join("index"), b"INDEXv0").unwrap();
        fs::write(git.join("refs").join("heads").join("main"), "a".repeat(40)).unwrap();
    }

    #[test]
    fn resolves_normal_git_dir_from_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_git(root);
        let sub = root.join("src").join("deep");
        fs::create_dir_all(&sub).unwrap();

        // Walk-up from a nested subdirectory finds the same git dir.
        let (git_dir, common_dir) = resolve_git_dirs(&sub).unwrap();
        assert_eq!(git_dir, root.join(".git"));
        assert_eq!(common_dir, git_dir, "normal repo: common dir == git dir");
    }

    #[test]
    fn resolves_worktree_pointer_and_commondir() {
        let tmp = tempfile::tempdir().unwrap();
        let main_git = tmp.path().join("main").join(".git");
        let wt_git = main_git.join("worktrees").join("feat");
        fs::create_dir_all(&wt_git).unwrap();
        fs::create_dir_all(&main_git).unwrap();
        // The worktree checkout: `.git` is a pointer file, not a directory.
        let wt = tmp.path().join("feat");
        fs::create_dir_all(&wt).unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", wt_git.to_string_lossy()),
        )
        .unwrap();
        // git dir's commondir points back at the main .git (relative form).
        fs::write(wt_git.join("commondir"), "../..\n").unwrap();

        let (git_dir, common_dir) = resolve_git_dirs(&wt).unwrap();
        assert_eq!(git_dir, wt_git);
        // ../../ from main/.git/worktrees/feat resolves to main/.git.
        assert_eq!(common_dir, wt_git.join("..").join(".."));
    }

    #[test]
    fn not_a_repo_resolves_to_none() {
        // A directory with no `.git` entry is not a git dir.
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_dot_git(&tmp.path().join(".git")).is_none());
        // And a path with no repo anywhere above it resolves to None.
        assert!(resolve_git_dirs(&no_repo_path()).is_none());
    }

    #[test]
    fn signature_changes_on_head_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_git(root);
        let git = root.join(".git");
        let before = repo_signature(&git, &git);

        // Same-length branch name: only HEAD's *contents* differ, so this
        // exercises the content hash rather than size/mtime.
        fs::write(git.join("HEAD"), "ref: refs/heads/side\n").unwrap();
        assert_ne!(before, repo_signature(&git, &git));
    }

    #[test]
    fn signature_changes_on_index_and_refs_and_packed_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_git(root);
        let git = root.join(".git");

        // Staging rewrites the index (grows it here).
        let s0 = repo_signature(&git, &git);
        fs::write(git.join("index"), b"INDEXv0-with-more-entries").unwrap();
        let s1 = repo_signature(&git, &git);
        assert_ne!(s0, s1, "index change must be detected");

        // A commit moves the branch ref (loose ref content/length changes).
        fs::write(git.join("refs").join("heads").join("main"), "b".repeat(41)).unwrap();
        let s2 = repo_signature(&git, &git);
        assert_ne!(s1, s2, "loose-ref change must be detected");

        // Packing refs creates packed-refs where there was none.
        fs::write(git.join("packed-refs"), "# pack-refs with: peeled\n").unwrap();
        let s3 = repo_signature(&git, &git);
        assert_ne!(s2, s3, "new packed-refs must be detected");
    }

    #[test]
    fn signature_stable_when_nothing_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_git(root);
        let git = root.join(".git");
        assert_eq!(repo_signature(&git, &git), repo_signature(&git, &git));
    }

    #[test]
    fn watch_reports_only_after_external_change() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_git(root);
        let w = GitWatcher::new();

        w.watch(7, &root.to_string_lossy());
        assert_eq!(w.len(), 1);
        // Freshly registered: nothing has moved, so no refresh is due.
        assert!(w.poll_changed().is_empty());

        // An external checkout (HEAD content) must surface exactly once.
        fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/side\n").unwrap();
        assert_eq!(w.poll_changed(), vec![7]);
        assert!(w.poll_changed().is_empty(), "same state must not re-fire");
    }

    #[test]
    fn watch_repointed_to_same_repo_keeps_pending_change() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_git(root);
        let w = GitWatcher::new();
        w.watch(1, &root.to_string_lossy());

        // Change happens, then a prompt re-registers the same repo (subdir cd)
        // before the next poll: the pending change must not be swallowed.
        fs::write(root.join(".git").join("index"), b"INDEXv1-changed").unwrap();
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();
        w.watch(1, &sub.to_string_lossy());
        assert_eq!(w.poll_changed(), vec![1]);
    }

    #[test]
    fn watch_outside_repo_and_unwatch_clear_the_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_git(root);
        let w = GitWatcher::new();

        w.watch(3, &root.to_string_lossy());
        assert_eq!(w.len(), 1);
        // cd out of the repo: the watch is dropped so we stop stat-ing.
        w.watch(3, &no_repo_path().to_string_lossy());
        assert_eq!(w.len(), 0);

        w.watch(3, &root.to_string_lossy());
        w.unwatch(3);
        assert_eq!(w.len(), 0);
    }
}
