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
// serialized too. Keying by the `repo` argument would confine the wait to one
// worktree, and for the `index.lock` hazard alone that would even be sound:
// each linked worktree has its own index. It is NOT sound for the rest —
// worktrees of one repo share `refs/` and `packed-refs`, where two
// `fetch --prune`s contend on `packed-refs.lock`, and two panes on two
// worktrees of one repo is loomux's standard shape rather than an edge case. A
// narrower key would trade a visible, bounded wait for an occasional error
// nobody can explain. The cost of the wider key is named below, not waved away.
//
// What this does NOT cover — two writers inside this very window:
//
//   1. The pane's own terminal. An agent running `git commit` in the worktree
//      the view is showing is a plain external process; loomux never excluded
//      it, before or after #726.
//   2. loomux's own backend. `Registry::spawn_agent_ex` calls
//      `git::git_worktree_add_sync` directly from its own thread
//      (orchestration/mod.rs), never through a command and so never through
//      this queue. It bypassed the freeze exactly the same way, so this is not
//      new — but it is inside the window, so "the window is serialized" would
//      be false, and this list exists to stop that from being claimed.
//
// So `index.lock` plus a surfaced error was always the real arbiter. This
// restores what the freeze provided and no more.
//
// The residual cost, stated because it is real: a slow head job delays every
// later mutating op, window-wide, including ones for unrelated repos. That is
// the same ordering the main thread imposed, and strictly better than the
// freeze it replaces (which killed the whole window for the same duration) —
// but the freeze was unmissable and a queue is silent, so the wait is bounded
// below, and the git view says so when it has to wait.

/** How long a job may sit unstarted before it is abandoned rather than run.
 *
 *  Bounding this is not optional. `run_git` spawns with no timeout, and
 *  `GIT_TERMINAL_PROMPT=0` suppresses a *terminal* credential prompt, not a GUI
 *  credential helper — so `fetch`/`push`/`pull` against an unreachable remote
 *  can genuinely never return, and an unbounded queue behind one of those is a
 *  suppression with no answer for "the signal never clears" — INV-6 in
 *  `doc/design/performance.md` ("any suppression driven by a fallible signal
 *  has a release that does not depend on that signal"), the invariant behind
 *  #496, #513 and #518. The release here does not consult the head job at all:
 *  it is this job's own elapsed wait.
 *
 *  Why abandon rather than release: releasing the waiter would let its `git`
 *  spawn run *concurrently* with the stuck one, which is precisely the race
 *  this queue exists to prevent. The bound must fail the job, never smuggle it
 *  past the guard — so the caller gets an explicit error saying the op did not
 *  run, which beats a click that vanishes.
 *
 *  Why a minute: every non-network command in the mutating set is sub-second on
 *  any normal tree, and even a slow `fetch` is seconds, so a wait past a minute
 *  means the head is pathological rather than merely slow. A legitimately long
 *  push can still trip it; the queued op then fails loudly and is retried once
 *  the push lands, which beats both hanging forever and racing it. */
export const QUEUE_WAIT_LIMIT_MS = 60_000;

/** Message a job is rejected with when it waited past the limit. Says plainly
 *  that nothing ran, so a caller's toast can never be read as "it failed". */
export const WAITED_TOO_LONG =
  "not started: another git operation has been running for over a minute. " +
  "Nothing was changed — try again once it finishes.";

export class SerialQueue {
  /** Resolves when the last enqueued job has settled. Deliberately typed as a
   *  never-rejecting promise: see `run`. */
  private tail: Promise<void> = Promise.resolve();

  /** True while a job's `work` is actually in flight. */
  private running = false;

  /** Whether a job is running right now — i.e. whether a job enqueued at this
   *  instant would have to wait. The git view uses it to say so, because its
   *  `act()` paths (file rows, context-menu items) have no button to spin. */
  get busy(): boolean {
    return this.running;
  }

  /** Enqueue `work`; it starts only once every previously enqueued job has
   *  settled, and the returned promise is `work`'s own — same value, same
   *  rejection, so a caller's contract is unchanged and only the timing moves.
   *
   *  If its turn has not come within `waitLimitMs`, the job is abandoned: the
   *  returned promise rejects with `WAITED_TOO_LONG` and `work` is never
   *  called, not even later. Pass `Infinity` to opt out. The clock covers the
   *  WAIT only — once `work` starts it may take as long as it takes, so a
   *  genuinely slow push is never cut off mid-flight. */
  run<T>(work: () => Promise<T>, waitLimitMs: number = QUEUE_WAIT_LIMIT_MS): Promise<T> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let abandoned = false;

    const started = this.tail.then(() => {
      // Our turn came, but too late: the caller has already been told this
      // never ran, so honour that rather than firing a surprise `git` spawn
      // minutes after the click.
      if (abandoned) throw new Error(WAITED_TOO_LONG);
      clearTimeout(timer);
      this.running = true;
      return work();
    });
    // The chain we keep for the NEXT job swallows both outcomes. A rejected job
    // must not wedge the queue (every later click would reject without ever
    // running), and the internal chain must not surface as an unhandled
    // rejection when the caller has already handled the promise we returned.
    const clear = (): void => {
      this.running = false;
    };
    this.tail = started.then(clear, clear);

    if (!Number.isFinite(waitLimitMs)) return started;
    return new Promise<T>((resolve, reject) => {
      timer = setTimeout(() => {
        abandoned = true;
        reject(new Error(WAITED_TOO_LONG));
      }, waitLimitMs);
      // Settle from the job itself whenever it gets that far. A `reject` after
      // the timer already fired is a no-op, so the first outcome wins.
      started.then(
        (v) => {
          clearTimeout(timer);
          resolve(v);
        },
        (e) => {
          clearTimeout(timer);
          reject(e);
        }
      );
    });
  }
}
