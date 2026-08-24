// Pure, DOM-free core of the visibility policy for EVENT-DRIVEN views (#1318).
// The sibling of pollgate.ts, for the other half of `doc/design/performance.md`
// §3: pollgate.ts answers "this view has a TIMER — should it tick?", and this
// answers "this view has an EVENT STREAM — should a wake run a refresh?".
//
// WHY THE TWO NEEDED SEPARATING. `pane.ts`'s `EmbedEntry.hide` already carried
// a rule, and it was written in terms of the mechanism rather than the
// question: "stops the follow poll on close/eviction ... which every POLLING
// view has to answer for". `TasksView` and `DecisionsView` have no timer at
// all, so nobody read that sentence as being about them — and they registered
// no `hide` hook. The result was that every `write_tasks` by every agent (plus
// every questions/needs-you write) drove a full refetch-and-rebuild in every
// board and every NEEDS-YOU panel that had EVER been opened on any pane in the
// session, on screen or not. The rule restated so it covers both: **a view that
// can be woken from outside must say what happens when nobody is looking at
// it** — the waker being a `setInterval` or a Tauri `listen` is an
// implementation detail of the waking, not of the rule.
//
// SUPPRESS, DON'T CATCH UP. Unlike `PollGate`, this gate owes no catch-up run:
// the pane calls `show()` on every open in either hosting mode, and both views'
// `show()` already ends in a `refresh()`. So a wake dropped while hidden is
// re-earned by the act of looking — no staleness can survive being looked at,
// which is why the gate needs no missed-wake bookkeeping and no trailing run.
// The cost it accepts in exchange is named in doc/design/embedded-panels.md: a
// reopened panel shows its last render for one backend round-trip before
// repainting, where before it was already current.
//
// THE RELEASE IS NOT THE EVENT (the standing rule that a suppression driven by
// a fallible signal needs a release that does not depend on that signal —
// #496/#513/#518, and pollgate.ts's own note). `wake()`/`sleep()` are a LATCH
// driven by two calls the pane makes, and a latch that misses its release is a
// panel that silently stops updating while the human is looking straight at
// it. So the gate never decides from the latch alone: `confirm` is the pane's
// own authoritative "is this view on screen right now?" read, consulted on
// exactly the path where being wrong is expensive, and a wake that arrives
// while the latch says asleep but `confirm` says visible RUNS — and re-wakes
// the latch, so the gate self-heals instead of paying that read forever.
//
//   latch  | confirm() | outcome
//   -------+-----------+--------------------------------------------------
//   awake  | not read  | run       (union: one true input is enough)
//   asleep | true      | run, re-wake the latch, count a stray
//   asleep | false     | suppress
//
// The union direction is deliberate. A missed `sleep()` (latch awake, panel
// hidden) costs exactly what the pre-#1318 code cost — a refresh nobody sees —
// while a missed `wake()` would show a human stale data, so the gate errs
// toward refreshing. `confirm` is one map lookup and one `.hidden` read: no
// layout, no IPC, and it is only ever reached on a wake the gate is about to
// suppress anyway.

/** The pane's live read of whether this view is on screen, in EITHER hosting
 *  mode. Injected rather than read off the DOM here so the policy stays
 *  DOM-free and the tests drive all four crossings directly. Must be cheap and
 *  must not force layout — it is called once per suppressed wake. */
export type VisibleProbe = () => boolean;

/** Counters behind `wakeGateStats()`. Module-level rather than a live-gate
 *  registry (pollgate.ts's shape): a gate has nothing to tear down, so keeping
 *  no registry means a disposed view cannot leave one behind. */
let deliveredCount = 0;
let suppressedCount = 0;
let strayCount = 0;

export interface WakeGateStats {
  /** Wakes that ran a refresh. */
  delivered: number;
  /** Wakes dropped because the view was off screen — the whole point. */
  suppressed: number;
  /** Wakes that ran because `confirm` contradicted the latch. **Expected to
   *  stay 0.** Anything else means a path makes a view visible without going
   *  through `Pane.openView` — i.e. the latch has a hole, and this is the
   *  instrument that says so rather than the panel quietly going stale. */
  strays: number;
}

/** The hand-validation instrument, reachable from devtools as
 *  `__wakeGateStats()` — the same affordance `__pollGateStats()` gives the
 *  timer half, and for the same reason: agents cannot run the GUI, so this is
 *  how the human sees whether the DOM wiring does what these unit tests say the
 *  policy does. Open a board, let agents write, close it: `suppressed` must
 *  climb while `delivered` stops, and `strays` must stay 0. */
export function wakeGateStats(): WakeGateStats {
  return { delivered: deliveredCount, suppressed: suppressedCount, strays: strayCount };
}

(globalThis as unknown as { __wakeGateStats?: () => WakeGateStats }).__wakeGateStats = wakeGateStats;

/** Reset the module-level counters. Test-only — they are cumulative by design
 *  so a human reading `__wakeGateStats()` sees a whole session. */
export function resetWakeGateStats(): void {
  deliveredCount = 0;
  suppressedCount = 0;
  strayCount = 0;
}

/** Runs an event-driven view's refresh only while that view is on screen.
 *
 *  Born asleep: every embed view in `pane.ts` is constructed lazily and the
 *  construction is always followed by the `openView` that shows it, so the
 *  first `wake()` arrives before anything can ask. Starting awake would instead
 *  leave a view that is built-but-never-shown refreshing forever — which is
 *  reachable today, since `Pane.requestEmbedFocus` constructs a view before
 *  deciding whether it can open one. */
export class WakeGate {
  private latch = false;
  private readonly confirm: VisibleProbe;

  /** @param confirm the pane's live visibility read (see `VisibleProbe`).
   *
   *  (An assigned field rather than a TypeScript parameter property: the node
   *  test runner strips types without transforming, and refuses that syntax —
   *  the same note `CoalescingRefresh` carries.) */
  constructor(confirm: VisibleProbe) {
    this.confirm = confirm;
  }

  /** The pane has made this view visible (`EmbedEntry.show`). */
  wake(): void {
    this.latch = true;
  }

  /** The pane is about to hide this view (`EmbedEntry.hide`) — a close, a slot
   *  eviction, an un-dock, in either hosting mode. */
  sleep(): void {
    this.latch = false;
  }

  /** Whether the gate currently believes the view is off screen. For the stats
   *  instrument and the tests; never a substitute for `accepts()`, which is the
   *  only thing that consults `confirm`. */
  get asleep(): boolean {
    return !this.latch;
  }

  /** A wake arrived — an event-stream notification, a deferred refresh, a
   *  human's own gesture. Returns whether the view should actually refresh.
   *
   *  Every wake goes through here by one rule, deliberately: splitting
   *  "event-driven wakes are gated, gesture-driven ones are not" would be a
   *  second rule to keep in step with the first, and a gesture inside a view
   *  the human cannot see is not a thing that happens. */
  accepts(): boolean {
    if (this.latch) {
      deliveredCount++;
      return true;
    }
    if (this.confirm()) {
      // Visible after all: run it, and heal the latch so the next wake takes
      // the cheap path instead of re-reading forever.
      this.latch = true;
      strayCount++;
      deliveredCount++;
      return true;
    }
    suppressedCount++;
    return false;
  }
}
