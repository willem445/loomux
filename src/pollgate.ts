// Pure, DOM-free core of the visibility policy for fixed-cadence polls
// (#743 S6). The DOM wiring lives in the views that own a poll — tabbar.ts,
// groupview.ts, auditview.ts, timelineview.ts — and everything that DECIDES
// anything is here so it is unit-testable under `node --test`, the same split
// as panethrottle.ts / refreshgate.ts.
//
// WHAT THIS IS FOR. `doc/design/performance.md` §3 INV-4: a frontend timer
// that drives IPC or rendering is visibility-aware or argued. The census
// (#743 plan part 2b §3) found nothing in `src/` that had ever read
// `document.hidden`, so a minimized window kept paying the tab strip's 4 s
// `groupSummary` + `groupUsage` sweep over every group-bound tab, forever, plus
// whatever panels were left open behind it — the group view's nine invokes
// every 2 s, an armed audit/timeline follow every 1.5 s. None of that feeds
// anything a human can see while the window is not on screen.
//
// PAUSE, NOT SKIP. The gate stops the underlying `setInterval` rather than
// letting it fire into a body that returns early. A tick that fires and
// discards still costs a timer wake and a callback on the one thread that also
// services input and paint (§1), and — the part that actually matters — an
// early return is a thing each caller has to remember to write, in the middle
// of a body that already has three other reasons to bail. Arming and disarming
// is a property of the timer, so it lives with the timer.
//
// THE RELEASE IS NOT THE EVENT (§2 P4, and the repo's standing rule that any
// suppression driven by a fallible signal needs a release that does not depend
// on that signal — #496/#513/#518). `visibilitychange` is a browser-owned
// notification: a listener that never fires leaves a poll suppressed forever,
// which for the group view means a panel that silently stops updating. So the
// gate does not trust the event to be the only way back. While a wanted poll is
// suppressed it runs a recheck ticker that READS the current visibility state
// (`HIDDEN_RECHECK_MS`) and resumes on what it reads, never on having been
// told. The recheck costs one property read per wake and issues no IPC, so a
// hidden window still makes zero data polls — which is the observable S6 is
// meant to produce.
//
// WHY THE CADENCE STAYS AT THE CALL SITE. The gate deliberately does not own
// the interval: each view keeps its own interval call, with its own cadence
// constant, and hands the gate three verbs. That keeps every shipped cadence a
// literal at the site that chose it, which is exactly what E2's timer manifest
// (`test/perfpolicy.test.ts`) scans for — a gate that swallowed the cadences
// into one file would make INV-4's enumeration blind in the act of satisfying
// it.

/** The three verbs a view hands the gate. All three must be idempotent enough
 *  to be called in any order the gate finds itself in: `arm` may be called when
 *  already armed (the view's own defensive clear-before-arm covers it), and
 *  `disarm` may be called when nothing is armed. */
export interface Poll {
  /** Start the view's own timer. Called only while the window is visible. */
  arm(): void;
  /** Stop it. */
  disarm(): void;
  /** One immediate run — the catch-up when a hidden window comes back, so the
   *  view is not showing data that is as stale as the hidden stretch was long.
   *  Never called on the first arm: a view that has just opened has already
   *  loaded once, and re-running that here would double every open. */
  refresh(): void;
}

/** Cancels something the gate started. */
export type Cancel = () => void;

/** Where the gate learns about visibility. Injected so the policy is DOM-free
 *  and so the tests drive transitions directly instead of faking a document. */
export interface VisibilitySource {
  /** The CURRENT state, read on demand. This is the half that makes the
   *  release bounded: it does not depend on any event having been delivered. */
  visible(): boolean;
  /** Subscribe to change notifications; returns the unsubscribe. Best-effort
   *  by construction — the recheck ticker is what covers it not arriving. */
  subscribe(onChange: () => void): Cancel;
}

/** Starts the suppressed-state recheck and returns its canceller. Injected in
 *  tests; the shipped implementation is `browserRecheck` below. */
export type RecheckTicker = (onWake: () => void) => Cancel;

/** How often a suppressed poll re-reads visibility instead of waiting to be
 *  told (see the header). 5 s is chosen against what it costs to be WRONG, not
 *  against a human's sense of "live": if the change event does arrive — the
 *  normal path — this ticker never fires at all, and if it does not, five
 *  seconds is the worst-case extra staleness on a window that has just come
 *  back. A wake is one boolean read and no IPC. */
export const HIDDEN_RECHECK_MS = 5000;

const browserRecheck: RecheckTicker = (onWake) => {
  const id = setInterval(onWake, HIDDEN_RECHECK_MS);
  return () => clearInterval(id);
};

/** The slice of `document` the gate needs — narrow on purpose, so a test can
 *  pass an object literal and the module still never touches a global. */
export interface VisibilityDocument {
  visibilityState: string;
  addEventListener(type: "visibilitychange", cb: () => void): void;
  removeEventListener(type: "visibilitychange", cb: () => void): void;
}

/** The shipped `VisibilitySource`: `document.visibilityState` plus the
 *  `visibilitychange` event. `visibilityState` rather than `document.hidden`
 *  because the two are defined off each other and only one of them says which
 *  state it read. */
export function documentVisibility(doc: VisibilityDocument): VisibilitySource {
  return {
    visible: () => doc.visibilityState !== "hidden",
    subscribe: (onChange) => {
      doc.addEventListener("visibilitychange", onChange);
      return () => doc.removeEventListener("visibilitychange", onChange);
    },
  };
}

let sharedSource: VisibilitySource | null = null;

/** The one source every gate shares by default, created on first use so that
 *  importing this module from Node (the unit tests) never touches `document`. */
function defaultVisibility(): VisibilitySource {
  if (sharedSource === null) sharedSource = documentVisibility(document);
  return sharedSource;
}

export interface PollGateOptions {
  visibility?: VisibilitySource;
  recheck?: RecheckTicker;
}

/** Counters behind `pollGateStats()`. Live gates only — `disable()` removes a
 *  gate from the registry, so a disposed view leaves nothing behind. */
const LIVE = new Set<PollGate>();
let resumeCount = 0;
let suppressCount = 0;

export interface PollGateStats {
  /** Gates whose view currently wants to poll (panel open, follow on, …). */
  enabled: number;
  /** Of those, the ones actually running — 0 while the window is hidden is the
   *  whole point of the slice. */
  armed: number;
  /** How many times a poll has been suppressed by the window going hidden. */
  suppressions: number;
  /** How many times one has been resumed. Lagging `suppressions` by more than
   *  one across a show/hide cycle is the shape a wedged gate would have. */
  resumes: number;
}

/** The hand-validation instrument #743 S6 asks for, reachable from devtools as
 *  `__pollGateStats()`: minimize the window and `armed` must fall to 0 with
 *  `suppressions` up by the number of live gates; restore it and `armed` must
 *  come back. Agents cannot run the GUI, so this is how the human sees whether
 *  the wiring does what the unit tests say the policy does. */
export function pollGateStats(): PollGateStats {
  let armed = 0;
  for (const gate of LIVE) if (gate.armed) armed++;
  return { enabled: LIVE.size, armed, suppressions: suppressCount, resumes: resumeCount };
}

(globalThis as unknown as { __pollGateStats?: () => PollGateStats }).__pollGateStats = pollGateStats;

/** Runs a view's poll only while the window is visible.
 *
 *  Two independent conditions decide whether the timer is armed, and both must
 *  hold: `wanted` (the view's own scope — panel open, follow toggled on) and
 *  the window being visible. Component scope and visibility are different
 *  questions and neither substitutes for the other, which is precisely the gap
 *  the census recorded: every poll here was already component-scoped, and every
 *  one of them still ran behind a minimized window. */
export class PollGate {
  private poll: Poll;
  /** Resolved on first `enable()`, not in the constructor: the views hold their
   *  gate as a class field, and constructing one must not reach for `document`
   *  before anyone has asked it to do anything. */
  private source: VisibilitySource | null;
  private recheck: RecheckTicker;
  private wanted = false;
  private running = false;
  /** Set when a wanted poll is held back, so the arm that follows knows it owes
   *  a catch-up `refresh()`. Not derivable from `wanted`/`running` alone: the
   *  first arm after `enable()` must NOT refresh (the view has just loaded),
   *  and the arm after a hidden stretch must. */
  private owesRefresh = false;
  private unsubscribe: Cancel | null = null;
  private cancelRecheck: Cancel | null = null;

  constructor(poll: Poll, opts: PollGateOptions = {}) {
    this.poll = poll;
    this.source = opts.visibility ?? null;
    this.recheck = opts.recheck ?? browserRecheck;
  }

  private visibility(): VisibilitySource {
    if (this.source === null) this.source = defaultVisibility();
    return this.source;
  }

  /** Whether the view's timer is running right now. */
  get armed(): boolean {
    return this.running;
  }

  /** The view wants to poll: it just opened, or its follow toggle went on.
   *  Arms immediately if the window is visible; otherwise waits, without ever
   *  having armed a timer nobody can see the results of. */
  enable(): void {
    if (this.wanted) return;
    this.wanted = true;
    LIVE.add(this);
    this.unsubscribe = this.visibility().subscribe(() => this.sync());
    this.sync();
  }

  /** The view is done polling: closed, hidden, follow toggled off, disposed.
   *  Tears down everything the gate started — the timer through `disarm()`, the
   *  visibility subscription, and any recheck ticker — so a disposed view
   *  leaves nothing running. Idempotent. */
  disable(): void {
    if (!this.wanted) return;
    this.wanted = false;
    LIVE.delete(this);
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.stopRecheck();
    this.owesRefresh = false;
    if (this.running) {
      this.running = false;
      this.poll.disarm();
    }
  }

  /** Reconcile the timer with (wanted × visible). The only place that arms or
   *  disarms, so the two callers — the change event and the recheck ticker —
   *  cannot drift from each other: both just say "look again". */
  private sync(): void {
    const visible = this.visibility().visible();
    const shouldRun = this.wanted && visible;
    // Any stretch in which the view wanted to poll and did not is a stretch of
    // missed ticks, so the arm that ends it owes a catch-up — including the
    // stretch that starts at an `enable()` made while the window was already
    // hidden, which never transitions and would otherwise arm into stale data.
    if (this.wanted && !visible) this.owesRefresh = true;
    if (shouldRun === this.running) {
      // Nothing to move. The one thing that still has to hold: a suppressed
      // poll keeps its recheck ticker, and a running one has none.
      this.syncRecheck();
      return;
    }
    this.running = shouldRun;
    if (shouldRun) {
      this.poll.arm();
      if (this.owesRefresh) {
        this.owesRefresh = false;
        resumeCount++;
        this.poll.refresh();
      }
    } else {
      suppressCount++;
      this.poll.disarm();
    }
    this.syncRecheck();
  }

  private syncRecheck(): void {
    const wantRecheck = this.wanted && !this.running;
    if (wantRecheck && this.cancelRecheck === null) {
      this.cancelRecheck = this.recheck(() => this.sync());
    } else if (!wantRecheck) {
      this.stopRecheck();
    }
  }

  private stopRecheck(): void {
    this.cancelRecheck?.();
    this.cancelRecheck = null;
  }
}

/** Reset the module-level counters. Test-only — the counters are cumulative by
 *  design so a human reading `__pollGateStats()` sees a whole session. */
export function resetPollGateStats(): void {
  resumeCount = 0;
  suppressCount = 0;
}
