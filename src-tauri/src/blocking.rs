//! The one delegation helper the gesture commands share — P1 of
//! `doc/design/performance.md`, applied to the modules #746 converts
//! (`pty`, `fileedit`, `filemgr`, `uistate`, `sessions`, `cliprobe`,
//! `editor`, `voice`, `gitwatch`).
//!
//! **Why this is a module and not a tenth copy.** `git.rs`, `gh.rs` and
//! `orchestration/mod.rs` each grew their own private `run_blocking` as their
//! own conversion landed (#399, #724, #762), which was the right size at three.
//! #746 converts twenty-five commands across nine more modules, and nine more
//! identical copies is how a mechanism drifts: one of them eventually
//! swallows a join failure, or logs instead of re-raising, and nothing says
//! which shape is the real one. The three existing copies stay where they are —
//! `git.rs`'s and `gh.rs`'s flatten a `Result` with a domain-specific message
//! and carry arguments (`git.rs`'s queue residue, #754) that are about those
//! modules, not about delegation — so folding them in here is a separate
//! change, not a drive-by in a perf slice.
//!
//! **And the two raw `spawn_blocking` call sites are left raw, deliberately.**
//! `sessions.rs` `list_sessions` and `voice.rs` `voice_stop` (#58) each
//! converted before this module existed and each does something with the join
//! failure that this helper would change: `voice_stop` maps it into its
//! `Result` as a domain error the frontend toasts, and `list_sessions` degrades
//! it to an empty list because every caller of that one is already written to
//! assume "best-effort, resumable on failure". Those are decisions with their
//! own doc comments, not boilerplate waiting to be deduplicated. #746 converted
//! commands in both of those files and did not touch their neighbours' error
//! handling on the way past.
//!
//! **That list was FOUR and is two (#1607).** `pty.rs` `write_pty` and
//! `change_dir` were the other two, raw since #734. They are not deduplicated
//! into this helper — they no longer reach the blocking pool at all. Epic #1600
//! Phase 2.3 moved the input path onto a thread per pane (P8-writer), because
//! this pool is a bounded shared resource and beta6 exhausted it with parked
//! orchestration poll ticks until keystrokes could not be scheduled (#1600
//! §1.2). Their bodies now go to `PtyManager::enqueue_frontend_write` /
//! `enqueue_cd`, and the command awaits the writer thread's completion reply.
//!
//! **What the command owes on top of calling this.** Delegation is only half of
//! INV-1: `spawn_blocking` moves the body, and Tauri still polls the future on
//! the webview thread up to the first `.await`, so anything a command does
//! before this call is still main-thread work (#724). Hand the WHOLE body over.
//!
//! And the half no helper can carry — **the reentrancy argument**. Synchronous
//! dispatch was an accidental mutual exclusion: one thread ran every command
//! body, so no two could ever interleave and nothing had to say why that was
//! safe. Moving a command off it is therefore a concurrency change, not just a
//! latency one. Every converted command in #746 carries a `**Reentrancy.**`
//! paragraph naming what serialized it before and what protects it now — a
//! lock, an atomic ticket, an idempotent write — or the interleaving it accepts
//! and why. Four of them needed a guard building rather than naming
//! (`uistate`'s write ordering, `fileedit`'s write mutex, `sessions`'s
//! launch-intent lock, `gitwatch`'s dispatch ticket); the rest already had one
//! and it simply became load-bearing.

/// Run `f` on the blocking pool and await it. A panicking body **stays a
/// panic**: it is re-raised here rather than degraded into an invented return
/// value, because moving work off the main thread must not change what a bug
/// does — and it changes nothing, since a sync command's panic already
/// unwound on this same call path. (`orchestration::run_blocking` makes the
/// same choice, for the same reason.)
pub async fn run_blocking<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(f).await {
        Ok(v) => v,
        Err(e) => panic!("blocking command task failed: {e}"),
    }
}
