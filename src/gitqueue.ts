// A FIFO serializer for the mutating git commands (#726).
//
// DOM-free and pure so its state machine is unit-tested in Node
// (test/gitqueue.test.ts) rather than by racing real invokes — the repo's
// extract-pure-logic convention (layout.ts, steer.ts, refreshgate.ts, …).
//
// Why it exists. Every git-shelling `#[tauri::command]` used to be synchronous,
// so Tauri ran it on the webview main thread and the window froze for the whole
// `git` run. #726 moved them all onto a blocking pool — but that freeze was also
// an accidental mutual exclusion: while the main thread sat in one `git` spawn,
// a second invoke could not start. Two concurrent `git` invocations against the
// same worktree contend on `index.lock`, and git fails the loser. Nothing gets
// corrupted, but a user who clicks *stage* then *commit* in quick succession
// used to get both (the second invoke queued behind the first) and would now
// get an error toast instead. So the ordering is restored deliberately, here,
// where the freeze used to provide it for free.
//
// Why one global queue and not one per repo. The exclusion being restored WAS
// global — the main thread is one thread, so writes to unrelated repos were
// serialized too. Keying by repo root would be narrower than the old behavior
// AND wrong at the edges: linked worktrees have distinct roots but share one
// `.git`, so a per-root key would let two of them race exactly where git's own
// locking is loosest. One queue reproduces the old ordering precisely, and the
// cost of the extra serialization is nil — waiting in a promise queue occupies
// no thread and freezes nothing, which is the entire point of #726.
//
// What this is NOT. It is a loomux-window guarantee, not a repo lock. The
// pane's own terminal — an agent running `git commit` in the very worktree the
// view is showing — has always been a concurrent writer that loomux never
// excluded, so `index.lock` plus a surfaced error was always the real arbiter.
// This restores what the freeze provided; it does not pretend to more.

export class SerialQueue {
  /** Resolves when the last enqueued job has settled. Deliberately typed as a
   *  never-rejecting promise: see `run`. */
  private tail: Promise<void> = Promise.resolve();

  /** Enqueue `work`; it starts only once every previously enqueued job has
   *  settled, and the returned promise is `work`'s own — same value, same
   *  rejection, so a caller's contract is unchanged and only the timing moves. */
  run<T>(work: () => Promise<T>): Promise<T> {
    const started = this.tail.then(work);
    // The chain we keep for the NEXT job swallows both outcomes. A rejected job
    // must not wedge the queue (every later click would reject without ever
    // running), and the internal chain must not surface as an unhandled
    // rejection when the caller has already handled the promise we returned.
    this.tail = started.then(
      () => undefined,
      () => undefined
    );
    return started;
  }
}
