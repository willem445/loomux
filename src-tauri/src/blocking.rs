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
//! **And the four call sites that hand off WITHOUT this helper still do,
//! deliberately** — they call [`spawn_counted`] rather than `run_blocking`.
//! (Until #1601 they called `tauri::async_runtime::spawn_blocking` directly and
//! this paragraph called them "raw"; Phase 0.3 routed every hand-off in the
//! crate through the one counted door, which changed what they call and not
//! what they DO with the result — which is what the paragraph is about.)
//! `pty.rs` `write_pty`/`change_dir` (#734), `sessions.rs` `list_sessions` and
//! `voice.rs` `voice_stop` (#58) each converted before this module existed and
//! each does something with the join failure that this helper would change:
//! two map it into their `Result` as a domain error the frontend toasts, and
//! `list_sessions` degrades it to an empty list because every caller of that
//! one is already written to assume "best-effort, resumable on failure". Those
//! are decisions with their own doc comments, not boilerplate waiting to be
//! deduplicated. #746 converted commands in all three of those files and did
//! not touch their neighbours' error handling on the way past.
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

/// **The one door onto the blocking pool** (#1601 Phase 0.3), and the only
/// `tauri::async_runtime::spawn_blocking` call left in `src-tauri/src`.
///
/// Returns exactly what awaiting a `spawn_blocking` handle returns, so it is a
/// substitution and not a policy: `run_blocking` below still re-raises a
/// panicked body, `gh.rs` and `git.rs` still flatten it into their own domain
/// error, and the four sites that skip `run_blocking` (`pty.rs`
/// `write_pty`/`change_dir`, `sessions.rs` `list_sessions`, `voice.rs`
/// `voice_stop`) still each do the thing this module's header says they
/// deliberately do. What changes is that
/// none of them reaches the runtime directly any more.
///
/// **Why one door rather than a counter at each site.** The count only means
/// anything if it is complete — a report reading `in-flight 480` is a
/// diagnosis, and one reading `in-flight 480 plus however many sites nobody
/// wrapped` is not. Eight hand-wrapped sites is a convention, checked by
/// whoever remembers; one door is a property a source scan can pin, and
/// `src-tauri/tests/selfwatch.rs` pins it.
///
/// **Where the ticket is taken is the whole point.** It is taken HERE, before
/// the hand-off, and moved into the task — so the depth counts work that is
/// still QUEUED as well as work that is running. A counter incremented inside
/// the closure would read 512 at saturation and go no higher, hiding the queue
/// behind it, which is the number the plan's §1.2 mechanism actually turns on.
pub async fn spawn_counted<T, F>(f: F) -> Result<T, tauri::Error>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let ticket = loomux_engine::selfwatch::pool_enter();
    tauri::async_runtime::spawn_blocking(move || {
        let _ticket = ticket;
        f()
    })
    .await
}

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
    match spawn_counted(f).await {
        Ok(v) => v,
        Err(e) => panic!("blocking command task failed: {e}"),
    }
}
