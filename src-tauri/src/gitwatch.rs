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
use tauri::{AppHandle, Emitter, Manager, State};

use crate::obs::LockExt;

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
    /// The newest dispatch ticket claimed for each pane — the ordering
    /// `git_watch`'s conversion had to restore (#746). See
    /// [`GitWatcher::claim`].
    ///
    /// **Lock order: `intents` before `watches`, never the reverse.**
    /// `watch_claimed` takes `intents` and acquires `watches` inside it to make
    /// its compare-and-insert one unit; `poll_changed` and `unwatch_claimed`'s
    /// removal take `watches` alone. Nothing takes `intents` while holding
    /// `watches`, so there is no cycle to find.
    ///
    /// It keeps one `(u32, u64)` per pane id ever watched, including panes long
    /// closed — that is the bound, stated rather than left to be discovered:
    /// twelve bytes per pane ever opened, for the life of the process. The
    /// alternative (dropping the entry on `unwatch`) is exactly the tombstone
    /// this exists to be — an in-flight `watch` would then see no newer intent
    /// and reinstall the watch it was racing.
    intents: Mutex<HashMap<u32, u64>>,
}

/// Hands out the tickets [`GitWatcher::claim`] stamps intents with. Process-wide
/// rather than per-watcher: there is one `GitWatcher`, and a global monotonic
/// counter cannot be reset by anything a test does to the registry.
static WATCH_TICKET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl GitWatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Point pane `id` at the repository containing `cwd`, or drop its watch if
    /// `cwd` is not inside a repo. Idempotent and cheap to call on every prompt:
    /// repointing at the same git dir keeps the stored signature, so no spurious
    /// refresh fires and a change that happened mid-interval is still caught on
    /// the next poll.
    ///
    /// The baseline `repo_signature` runs *outside* the lock (#743 S7), the way
    /// `poll_changed` below already stats outside it — snapshot the decision,
    /// release, do the I/O, re-acquire to store. This is the same file's own
    /// documented rule, which only this method was breaking: a hung stat on an
    /// unresponsive network drive stalled the pane that asked for the repoint
    /// *and* every other watch, including the poll thread's compare-and-store.
    ///
    /// **Reentrancy.** Three things can be concurrent with the released window,
    /// and none can corrupt it. The poll thread may compare-and-store while we
    /// are stat-ing: it guards on `git_dir`, so it can only ever update the
    /// signature of the watch that is *still* pointed where it read from, and
    /// our insert then replaces that whole entry with a baseline for the NEW
    /// repo. The `git_dir` re-check under the second acquisition covers a
    /// concurrent `watch` for the same pane, so "repointing at the same repo
    /// keeps the stored signature" holds by construction. And a concurrent
    /// `unwatch` — or a NEWER `watch` — is what `ticket` is for: see
    /// [`GitWatcher::claim`]. Until #746 that last one could not happen at all,
    /// because `git_watch`/`git_unwatch` were both sync `#[tauri::command]`s and
    /// the one webview thread serialised them.
    fn watch_claimed(&self, id: u32, cwd: &str, ticket: u64) {
        let Some((git_dir, common_dir)) = resolve_git_dirs(Path::new(cwd)) else {
            let intents = self.intents.lock_safe();
            if self.is_current(&intents, id, ticket) {
                self.watches.lock_safe().remove(&id);
            }
            return;
        };
        // Cheap check under a temporary guard, then release for the I/O.
        if self.watches.lock_safe().get(&id).is_some_and(|w| w.git_dir == git_dir) {
            return;
        }
        let last_sig = repo_signature(&git_dir, &common_dir);
        // `intents` first, then `watches` — the order this type's field doc
        // fixes. Held together so "still the newest intent" and the insert it
        // authorises cannot be separated by an `unwatch` landing between them.
        let intents = self.intents.lock_safe();
        if !self.is_current(&intents, id, ticket) {
            return; // superseded while we were stat-ing: a newer watch, or a close
        }
        let mut map = self.watches.lock_safe();
        if map.get(&id).is_some_and(|w| w.git_dir == git_dir) {
            return; // installed while we were stat-ing; its baseline is fresher
        }
        map.insert(
            id,
            Watch {
                git_dir,
                common_dir,
                last_sig,
            },
        );
    }

    /// Take the next dispatch ticket for pane `id` and stamp it as this pane's
    /// newest intent (#746).
    ///
    /// **What this replaces.** `git_watch` and `git_unwatch` were both sync
    /// commands, so Tauri ran them one at a time on the webview thread and
    /// their effects landed in arrival order for free. `git_watch` now does its
    /// stat-ing off that thread, and two things break with the ordering:
    ///
    ///   * a pane repointed twice in quick succession (two OSC-7 reports, two
    ///     `setGitWatch` calls) could install the OLDER cwd last, leaving the
    ///     git view pointed at the directory the user just left — with no
    ///     further event coming to correct it;
    ///   * worse, a pane CLOSED while a `watch` was in flight would have the
    ///     watch reinstalled after `git_unwatch` removed it, leaking an entry
    ///     that the poll thread then stats every second, forever, for a pane
    ///     that no longer exists.
    ///
    /// The fix is the ordering itself, not a lock: a ticket claimed **on the
    /// webview thread**, before the first `.await`, is stamped in arrival order
    /// — the same order sync dispatch used to impose — and a body that finds a
    /// newer intent for its pane simply declines to act. `unwatch` claims one
    /// too, which is what makes a close a tombstone rather than a gap.
    ///
    /// One map insert under a briefly-held leaf lock: the same in-memory work
    /// `git_unwatch` (a `cheap` row in E1's manifest) does entirely on this
    /// thread, and it is what lets everything after it move off.
    pub fn claim(&self, id: u32) -> u64 {
        let ticket = WATCH_TICKET.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.intents.lock_safe().insert(id, ticket);
        ticket
    }

    /// Is `ticket` still the newest intent claimed for `id`? Takes the guard as
    /// an argument rather than the lock, so a caller can hold the decision and
    /// the action it authorises together.
    fn is_current(
        &self,
        intents: &std::sync::MutexGuard<'_, HashMap<u32, u64>>,
        id: u32,
        ticket: u64,
    ) -> bool {
        // An absent entry reads as current: every path claims before it acts,
        // so this cannot happen — and if it somehow did, declining every write
        // would break the watcher silently, while allowing one degrades to the
        // pre-#746 behaviour.
        intents.get(&id).map_or(true, |newest| *newest == ticket)
    }

    /// Point pane `id` at `cwd`, claiming a ticket for it first. The shape
    /// every non-command caller wants; `git_watch` splits the two halves so the
    /// claim happens on the webview thread and the stat-ing does not.
    pub fn watch(&self, id: u32, cwd: &str) {
        let ticket = self.claim(id);
        self.watch_claimed(id, cwd, ticket);
    }

    /// Stop watching pane `id` (called when its pane is disposed).
    ///
    /// Claims a ticket of its own first — see [`GitWatcher::claim`]. That is
    /// what makes a close win against a `watch` still stat-ing: the in-flight
    /// body finds a newer intent and declines to reinstall.
    pub fn unwatch(&self, id: u32) {
        self.claim(id);
        self.watches.lock_safe().remove(&id);
    }

    /// Recompute every watch's signature and return the pane ids whose repo
    /// metadata moved since the last poll, updating the stored signatures.
    ///
    /// The filesystem I/O runs *outside* the lock: we snapshot the watch set,
    /// stat unlocked, then re-acquire only to compare and store. A hung stat
    /// (an unplugged or unresponsive network drive) therefore stalls that one
    /// poll but never blocks `git_watch`/`git_unwatch` or the other watches.
    pub fn poll_changed(&self) -> Vec<u32> {
        // Snapshot (id, dirs) under the lock, then release it for the I/O.
        let snapshot: Vec<(u32, PathBuf, PathBuf)> = {
            let map = self.watches.lock_safe();
            map.iter()
                .map(|(id, w)| (*id, w.git_dir.clone(), w.common_dir.clone()))
                .collect()
        };

        // Stat unlocked. Carry the git_dir so we can detect a repoint below.
        let sigs: Vec<(u32, PathBuf, u64)> = snapshot
            .into_iter()
            .map(|(id, git_dir, common_dir)| {
                let sig = repo_signature(&git_dir, &common_dir);
                (id, git_dir, sig)
            })
            .collect();

        // Re-acquire only to compare/store. Skip watches that were dropped or
        // repointed to a different repo while we were stat-ing: an unregistered
        // one is simply gone, and a repointed one already has a fresh baseline
        // signature from `watch()`, so the stale sig we computed is discarded.
        let mut changed = Vec::new();
        let mut map = self.watches.lock_safe();
        for (id, git_dir, sig) in sigs {
            if let Some(w) = map.get_mut(&id) {
                if w.git_dir == git_dir && sig != w.last_sig {
                    w.last_sig = sig;
                    changed.push(id);
                }
            }
        }
        changed
    }

    /// Number of active watches (test/introspection helper).
    #[cfg(test)]
    fn len(&self) -> usize {
        self.watches.lock_safe().len()
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

/// Point pane `id`'s watch at the repo containing `cwd`. Called on every prompt
/// (OSC 7), so it must stay cheap; it is idempotent for a repeat of the same
/// repo.
///
/// Off-thread (#746 — `crate::blocking::run_blocking`, P1 of
/// `doc/design/performance.md`): computing the repo signature stats several
/// files and reads `HEAD`, which Tauri ran on the thread that services paint.
/// #743 S7 had already moved that I/O out from under the watches mutex, so the
/// lock-scope half was done; this is the plain half.
///
/// `State<Arc<GitWatcher>>` becomes `AppHandle` + a managed-state lookup: a
/// borrowed `State` gives an async command a lifetime parameter, which Tauri
/// supports only for a `Result` return — and this command returns `()`, a
/// frozen wire contract the frontend's fire-and-forget `setGitWatch` is built
/// against. Both are Tauri-injected and neither appears in the argument object,
/// so the contract is byte-identical (the same reasoning `orchestration`'s
/// converted commands carry).
///
/// **Reentrancy.** The ticket is claimed HERE, before the first `.await`, so it
/// is stamped in arrival order — see [`GitWatcher::claim`] for the two orderings
/// that restores (a stale repoint winning, and a closed pane's watch being
/// reinstalled after `git_unwatch` removed it).
#[tauri::command]
pub async fn git_watch(app: AppHandle, id: u32, cwd: String) {
    let watcher = app.state::<Arc<GitWatcher>>().inner().clone();
    let ticket = watcher.claim(id);
    crate::blocking::run_blocking(move || watcher.watch_claimed(id, &cwd, ticket)).await
}

/// Stop watching pane `id` (pane dispose). Stays SYNC — two in-memory map
/// mutations under briefly-held leaf locks, no filesystem read at all, so E1
/// classifies it `cheap` and a thread hop would only add latency.
///
/// It claims a dispatch ticket as it goes (#746), which is the half that makes
/// a close beat a `git_watch` still stat-ing off-thread. That is not incidental
/// to it being sync: the claim happens on the webview thread precisely because
/// this command's whole body does.
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

        // A commit moves the branch ref to a new 40-char sha — the *same
        // length* as before, so this can only be caught via the stat/mtime
        // path (loose refs are stat-ed, not content-hashed). Pin a distinct
        // mtime afterwards so the assertion holds even where a fast rewrite
        // lands in the same filesystem clock tick.
        let ref_path = git.join("refs").join("heads").join("main");
        let t0 = fs::metadata(&ref_path).unwrap().modified().unwrap();
        fs::write(&ref_path, "b".repeat(40)).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&ref_path)
            .unwrap()
            .set_modified(t0 - std::time::Duration::from_secs(86_400))
            .unwrap();
        let s2 = repo_signature(&git, &git);
        assert_ne!(s1, s2, "constant-length ref update must be caught via mtime");

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
    fn watch_repointed_to_a_different_repo_takes_a_fresh_baseline() {
        // #743 S7 moved the baseline `repo_signature` out from under the
        // `watches` lock, so the stat now runs in a window where the map is
        // unlocked. The property that must survive that: a pane repointed at a
        // DIFFERENT repo is baselined against the repo it moved to, not the one
        // it left — otherwise the very next poll reports a spurious change for
        // a repo nothing touched.
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        init_git(&first);
        init_git(&second);
        let w = GitWatcher::new();

        w.watch(9, &first.to_string_lossy());
        w.watch(9, &second.to_string_lossy());
        assert_eq!(w.len(), 1, "a repoint replaces the watch, never adds one");
        assert!(
            w.poll_changed().is_empty(),
            "the new repo was baselined at the repoint, so nothing is due yet"
        );

        fs::write(second.join(".git").join("HEAD"), "ref: refs/heads/side\n").unwrap();
        assert_eq!(w.poll_changed(), vec![9], "and the new repo is the one being watched");
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

    #[test]
    fn a_close_beats_a_watch_that_was_still_stat_ing() {
        // #746's ordering pin, and the one with teeth: `git_watch` does its
        // stat-ing off the webview thread now, so a pane CLOSED mid-flight
        // would have its watch reinstalled after `git_unwatch` removed it —
        // an entry the poll thread then stats every second, forever, for a pane
        // that no longer exists. Splitting claim from body is exactly what the
        // command does, so driving the halves in this order IS the race, made
        // deterministic rather than hoped for.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_git(root);
        let w = GitWatcher::new();

        let ticket = w.claim(4); // git_watch's claim, on the webview thread
        w.unwatch(4); // the pane closes while that body is still stat-ing
        w.watch_claimed(4, &root.to_string_lossy(), ticket); // the body lands late
        assert_eq!(
            w.len(),
            0,
            "a watch that lost its claim to a close reinstalled itself — the poll thread now \
             stats a dead pane's repo for the life of the process"
        );
    }

    #[test]
    fn a_stale_repoint_never_overwrites_a_newer_one() {
        // The other ordering the ticket restores: a pane repointed twice in
        // quick succession must end up at the LAST cwd, not whichever body
        // finished last. Here the older body lands second on purpose.
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        init_git(&first);
        init_git(&second);
        let w = GitWatcher::new();

        let stale = w.claim(5); // the user cds to `first`…
        let newest = w.claim(5); // …then straight on to `second`
        w.watch_claimed(5, &second.to_string_lossy(), newest);
        w.watch_claimed(5, &first.to_string_lossy(), stale);
        assert_eq!(w.len(), 1);
        assert_eq!(
            w.watches.lock_safe().get(&5).map(|watch| watch.git_dir.clone()),
            Some(second.join(".git")),
            "the stale repoint won, leaving the git view pointed at the directory the user \
             just left — with no further event coming to correct it"
        );
    }

    #[test]
    fn an_uncontested_claim_still_installs_the_watch() {
        // Anti-vacuity for the two tests above: the ticket check must refuse a
        // superseded body, not every body. If it jammed on "not current" both
        // of those would pass while the watcher did nothing at all.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_git(root);
        let w = GitWatcher::new();

        let ticket = w.claim(6);
        w.watch_claimed(6, &root.to_string_lossy(), ticket);
        assert_eq!(w.len(), 1, "an unsuperseded claim must install its watch");
    }
}
