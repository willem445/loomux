// Pure, DOM-free core of the visibility policy for EVENT-DRIVEN views (#1318).
// The sibling of pollgate.ts, for the other half of `doc/design/performance.md`
// §3: pollgate.ts answers "this view has a TIMER — should it tick?", and this
// answers "this view has an EVENT STREAM — should a wake run a refresh?".
//
// WHY THE TWO NEEDED SEPARATING. Nothing on `EmbedEntry.hide` ever asked a new
// view this question. Its doc comment said the hook was for "extra per-view
// cleanup beyond hiding its host", and closed with "Optional: most views need
// nothing beyond the generic hide" — an invitation to skip it. The rule that
// would have caught this was real but misplaced: it sat three thousand lines
// below, as a comment on ONE view's registration ("stops the follow poll on
// close/eviction ... which every POLLING view has to answer for", on the
// `timeline` entry since #648), where nobody adding a NEW view has any reason
// to look. `TasksView` and `DecisionsView` were then registered with no `hide`
// hook at all, so every `write_tasks` by every agent (plus every
// questions/needs-you write) drove a full refetch-and-rebuild in every board
// and every NEEDS-YOU panel that had EVER been opened on any pane in the
// session. `AuditView` missed it a third time from the other direction: it DOES
// poll, and its poll was still left running behind a closed panel. The rule now
// lives on the interface, and is about the question rather than the mechanism:
// **a view that can be woken from outside must say what happens when nobody is
// looking at it** — the waker being a `setInterval` or a Tauri `listen` is an
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

/** The pane's live read of whether this view's PANEL is open, in EITHER hosting
 *  mode. Injected rather than read off the DOM here so the policy stays
 *  DOM-free and the tests drive all four crossings directly. Must be cheap and
 *  must not force layout — it is called once per suppressed wake.
 *
 *  **What it can and cannot see, because everything below turns on this.** The
 *  shipped probe is `Pane.isViewVisible`, which reads one element's own
 *  `hidden` attribute. That is exactly "is this panel open"; it is NOT "can a
 *  human see it". Two states put a view genuinely off screen with that
 *  attribute untouched, and the gate keeps working in both: a background
 *  project tab (`Workspace.setVisible(false)` sets `display:none` on an
 *  ancestor) and a minimized pane (`pane.el` is detached, not hidden). Neither
 *  calls any pane method that could move the latch. So the bound this module
 *  delivers is **"a closed panel costs one boolean"**, not "an off-screen one":
 *  a board left open in a background tab still pays the full refetch and
 *  rebuild on every agent write. That residual is #1465, and it is a real
 *  visibility signal pushed down from the tab/grid layer, not a clause — the
 *  cheap probes are all barred, since `offsetParent`/`getBoundingClientRect`
 *  force layout (the contract above) and `isConnected` catches the detached
 *  case but not `display:none`. */
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
  /** Wakes dropped because the view's panel was closed — the whole point. */
  suppressed: number;
  /** Wakes that ran because `confirm` contradicted the latch. **Expected to
   *  stay 0**, and worth being precise about what that proves: since `confirm`
   *  is `Pane.isViewVisible`, a stray can only be raised by a path that OPENS a
   *  panel without going through `Pane.openView`. A clean `strays` therefore
   *  says `openView`/`closeView` pairs balance — which `test/embedwake.test.ts`
   *  already argues structurally — and says NOTHING about the two states
   *  `VisibleProbe` is blind to (a background tab, a minimized pane; #1465).
   *  It is a latch-integrity counter, not a visibility one. */
  strays: number;
}

/** The hand-validation instrument, reachable from devtools as
 *  `__wakeGateStats()` — the same affordance `__pollGateStats()` gives the
 *  timer half, and for the same reason: agents cannot run the GUI, so this is
 *  how the human sees whether the DOM wiring does what these unit tests say the
 *  policy does. Open a board, let agents write, close it: `suppressed` must
 *  climb while `delivered` stops, and `strays` must stay 0.
 *
 *  These counters are MODULE-GLOBAL — every gate in every view in every pane in
 *  every tab adds to the same three numbers. So "close the board and `delivered`
 *  must stop" means close every board and every NEEDS-YOU panel in EVERY tab,
 *  including background ones; one left open elsewhere keeps `delivered`
 *  climbing and reads exactly like the fix not working. A per-pane breakdown is
 *  part of #1465. */
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
 *  Born asleep, because a latch starts in the state the pane has not yet
 *  asserted, and "not shown yet" is not "showing". No path reaches it today: I
 *  checked every construction site in `pane.ts` (`ensureEmbedView` and
 *  `restoreEmbeds` both construct only on branches that fall through to
 *  `openView`, and every `toggleXView` calls its `ensureXView` immediately
 *  before `toggleView`), so the first `wake()` always arrives before anything
 *  can ask. That is a property of today's call sites, not of this class — the
 *  default is what keeps a future construct-then-decide path from leaving a
 *  view refreshing forever, and `M5` pins it against being flipped. */
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

  /** Whether the gate currently believes the view's panel is closed. For the
   *  stats instrument and the tests; never a substitute for `accepts()`, which
   *  is the only thing that consults `confirm`. */
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
