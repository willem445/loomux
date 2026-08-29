// Pure, DOM-free core of the published-view staleness badge (#1608, plan
// #1600 §3 Phase 1). The DOM wiring lives in the views that render it —
// groupview.ts's header, tabbar.ts's strip — and everything that DECIDES
// anything is here so it is unit-testable under `node --test`, the same split
// as pollgate.ts / singleflight.ts / refreshgate.ts.
//
// WHAT THIS IS FOR — the silent freeze (#1604 review N3). #1602/#1604 made the
// blocking pool impossible to exhaust from the poll path by single-flighting
// both poll sites: a tick that finds its own previous call still outstanding
// skips instead of issuing another. That is the right liveness answer and it
// costs a disclosure. If the outstanding call NEVER settles — the stuck-lock
// case the gate exists for — the gate never clears and the panel keeps
// rendering its last payload forever: no badge, no timestamp, no toast. The
// human sees a group view that looks live and is not, which is worse than one
// that looks broken.
//
// Phase 1 removes the freeze at the source (a polled read now reads a
// published snapshot and cannot park) and this module is what makes the
// remaining case VISIBLE. A wedged registry parks exactly one thread — the
// publisher's — and every reader keeps answering with the last snapshot and a
// growing `age_ms`. Bounded, visible, recoverable: INV-6 applied to the
// registry.
//
// THE BADGE IS ENTERED ON THE CLOCK AND RELEASED ONLY ON EVIDENCE. The backend
// decides `stale` (from `age_ms > VIEW_STALE_AFTER_MS`, both computed
// backend-side at read time) and nothing clears it but the next successful
// publish, which is what re-stamps the payload. This module never runs a timer
// of its own and never decides staleness from the browser's clock: the two
// clocks disagree, and a badge that came down because a frontend timer expired
// would be exactly the "release on elapsed time rather than on independent
// evidence" failure `.orrerix/lessons.md` names.
//
// WHY `age_ms` RATHER THAN A TIMESTAMP DIFFERENCE. `meta.published_at_ms` is a
// wall-clock stamp carried for a human to read; `meta.age_ms` is measured
// backend-side from a MONOTONIC instant at the moment the read was served. A
// frontend subtracting `Date.now() - published_at_ms` would report a snapshot
// from the future after an NTP correction or a VM resume, and would silently
// bake the clock skew of every machine into the badge.

/** The `meta` block every published-view payload carries (`orch_group_view`,
 *  `orch_strip_view`). The wire contract is `doc/design/polled-views.md`. */
export interface ViewMeta {
  /** Monotonic publication counter. A reader that sees the same `seq` twice
   *  read the same publication twice. */
  seq: number;
  /** Wall-clock stamp of the publication, for display only — never subtract
   *  from it to decide staleness (see the header). */
  published_at_ms: number;
  /** How old the payload was when the backend served it, measured
   *  backend-side from a monotonic clock. */
  age_ms: number;
  /** What the publish pass that produced it cost, in ms. */
  compute_ms: number;
  /** `age_ms > VIEW_STALE_AFTER_MS`, decided backend-side so there is one
   *  definition of "stuck" in the app rather than two. */
  stale: boolean;
  /** Reserved for plan Phase 2.1: a section that hit a `Busy` timeout kept its
   *  previous value. Always `false` today — Phase 1 has no bounded acquisition
   *  to time out — and in the contract now so this renderer does not change
   *  shape when 2.1 lands. */
  partial: boolean;
}

/** `orch_group_view`'s meta additionally answers whether the eight view-tier
 *  sections are present. They are absent on the first read after a panel opens
 *  (the publisher has not picked the lease up yet) and after a lease lapses —
 *  never "present but defaulted", because a fabricated `paused: false` is a
 *  wrong answer rendered as a right one. */
export interface GroupViewMeta extends ViewMeta {
  view_ready: boolean;
}

/** What a view should render about its payload's freshness. */
export interface StaleState {
  /** Whether to show the badge at all. */
  stale: boolean;
  /** The badge text, or `""` when `stale` is false. */
  label: string;
  /** A longer explanation for the badge's `title`, or `""` when not stale. */
  detail: string;
}

const FRESH: StaleState = { stale: false, label: "", detail: "" };

/** How an age is written on the badge: whole seconds under a minute, then
 *  whole minutes, then hours. Deliberately coarse — the number is evidence
 *  that the panel has stopped moving, not a measurement, and a badge whose
 *  last digit changes every render is a badge that draws the eye away from the
 *  panel it is annotating. */
export function formatAge(ageMs: number): string {
  const ms = Number.isFinite(ageMs) && ageMs > 0 ? ageMs : 0;
  const secs = Math.floor(ms / 1000);
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  return `${hours}h`;
}

/** Decide the staleness badge from a payload's `meta`.
 *
 *  `null`/`undefined` meta means the payload itself was refused or has not
 *  arrived — the caller keeps its previous render, and this reports NOT stale
 *  rather than inventing a badge about a payload it never saw. A missing
 *  payload is a different condition from a stale one, and conflating them
 *  would put a permanent badge on a group id the backend simply does not know.
 *
 *  `stale` comes from the backend; this never re-derives it from `age_ms`.
 *  Two clocks and one threshold is how the two halves drift apart, and the
 *  backend is the half that knows when the snapshot was actually taken. */
export function staleState(meta: ViewMeta | null | undefined): StaleState {
  if (!meta || meta.stale !== true) return FRESH;
  const age = formatAge(meta.age_ms);
  if (meta.partial === true) {
    // Phase 2.1's shape: some sections are stale and the rest are current, so
    // the label must not claim the whole panel is frozen.
    return {
      stale: true,
      label: `partly stale ${age}`,
      detail:
        `Some of this panel could not be refreshed and is showing values from ${age} ago. ` +
        `The rest is current. It updates itself as soon as the backend answers again.`,
    };
  }
  return {
    stale: true,
    label: `stale ${age}`,
    detail:
      `This panel is showing a snapshot from ${age} ago — the backend has not been able to ` +
      `publish a newer one. Nothing is lost; it updates itself as soon as the backend answers ` +
      `again.`,
  };
}

/** Whether the caller should re-ask soon because the view tier has not been
 *  published yet — the first read after a panel opens, or after its lease
 *  lapsed and the tier was dropped.
 *
 *  This is a BOUNDED ladder, not a retry loop: the caller re-asks once on a
 *  short delay and then falls back to its normal cadence, so a backend that
 *  never publishes costs one extra call per panel open rather than a spin. The
 *  view tier is what the group view's eight remaining sections come from, so
 *  without this the panel renders empty for up to a full poll period on every
 *  open. */
export function needsViewTierRetry(meta: GroupViewMeta | null | undefined): boolean {
  return !!meta && meta.view_ready === false;
}

/** How long to wait before that one re-ask. Shorter than the publish interval
 *  (1000 ms) so a panel opened just after a pass does not wait out the next
 *  one, and long enough that the re-ask lands after the pass the lease stamp
 *  triggered. */
export const VIEW_TIER_RETRY_MS = 250;

// ---------- the tab strip's binding/disclosure witness ----------
//
// State, not DOM, so it lives here rather than in tabbar.ts and both of its
// paths are directly testable. That placement is the point: while it lived
// beside the sweep it was reachable only through a real strip talking to a
// real backend, so its failure-path blind spot (#1625 review round 2, B6)
// could be argued about but not reddened.
//
// Read by the E2E soak lane through `__tabStatusStats()`, which is the
// binding witness that replaced the IPC fan-out #1608 removed.

export interface TabStatusStats {
  /** Every tab whose persisted `groupIds` names a group. A fact about tabs,
   *  so it survives a failed read. */
  bound: string[];
  /** The subset the published snapshot actually carried. Empty after a
   *  failed read, because that sweep resolved nothing. */
  seen: string[];
  /** What the strip last disclosed about its own freshness — the same value
   *  the chips render, on BOTH paths. */
  stale: boolean;
  /** Age of the snapshot the strip last rendered. `Infinity` after a failed
   *  read: the last good snapshot's age is unknown and unbounded, and
   *  reporting the previous number would be the same lie in another field. */
  ageMs: number;
}

let sweep: TabStatusStats = { bound: [], seen: [], stale: false, ageMs: 0 };

/** A sweep that got a payload. */
export function recordSweepSuccess(
  bound: string[],
  seen: string[],
  stale: boolean,
  ageMs: number
): void {
  sweep = { bound: [...bound], seen: [...seen], stale, ageMs };
}

/** A sweep whose read threw.
 *
 *  Recording this is the whole of B6. The strip renders its stale badge on
 *  this path, so a witness that skipped it reported `stale: false` for a
 *  strip that was visibly stale — and the soak lane's "did it recover?"
 *  assertion reads exactly that field, so it passed in the state it exists to
 *  catch. */
export function recordSweepFailure(bound: string[]): void {
  sweep = { bound: [...bound], seen: [], stale: true, ageMs: Number.POSITIVE_INFINITY };
}

export function tabStatusStats(): TabStatusStats {
  return { bound: [...sweep.bound], seen: [...sweep.seen], stale: sweep.stale, ageMs: sweep.ageMs };
}

(globalThis as unknown as { __tabStatusStats?: () => TabStatusStats }).__tabStatusStats =
  tabStatusStats;
