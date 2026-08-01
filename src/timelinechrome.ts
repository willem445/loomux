// Chrome decisions for the progress-timeline view (#608, Slice C): the
// answers timelineview.ts needs that can be WRONG, pulled out of the DOM so
// they're unit-tested (test/timelinechrome.test.ts) instead of hand-checked.
// The SVG itself, the polling and the click wiring stay in the view, where the
// repo's convention leaves DOM work.
//
// Self-contained by the same rule as timelinemodel.ts / timelinelayout.ts: NO
// intra-src imports, not even type ones (TS5097 — see the note in
// embedsplit.ts), because `node --test` resolves this file directly. Category
// and kind arguments are therefore plain strings that the view passes from
// `categoryOf()`; structural typing means the real unions pass straight in.

/** How coarse the axis is, derived from the tick step the layout chose. The
 *  view maps this to `Intl.DateTimeFormat` options — locale lives in the view,
 *  never here. */
export type TickScale = "seconds" | "minutes" | "hours" | "days";

const MINUTE_MS = 60_000;
const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;

/** Pick the label granularity for a tick step.
 *
 *  This exists because the obvious alternative — one format for every window —
 *  is wrong in both directions: a 72h window whose ticks are 12h apart renders
 *  as a row of identical `00:00:00`s, and a 1h window labelled `Jul 30` says
 *  nothing at all. The boundaries are inclusive-below: a step of exactly one
 *  minute is a minute-scale axis, not a second-scale one. */
export function tickScale(stepMs: number): TickScale {
  if (!Number.isFinite(stepMs) || stepMs < MINUTE_MS) return "seconds";
  if (stepMs < HOUR_MS) return "minutes";
  if (stepMs < DAY_MS) return "hours";
  return "days";
}

/** Lane headings, keyed by `timelinemodel`'s `TimelineCategory`. */
export const LANE_LABELS: Readonly<Record<string, string>> = {
  group: "group",
  agents: "agents",
  work: "work",
  gates: "gates",
  github: "GitHub",
  ops: "ops",
};

/** The heading for a lane. An UNKNOWN key returns the key itself rather than
 *  an empty string: `layoutTimeline` appends a lane it has never seen instead
 *  of dropping it, so a category added to the model before this table catches
 *  up must render as a named lane, not a blank one. */
export function laneLabel(category: string): string {
  return LANE_LABELS[category] ?? category;
}

/** Flip one category chip.
 *
 *  Two decisions live here. The result is re-sorted into `order` so the chips
 *  and the lanes agree on top-to-bottom position no matter what sequence the
 *  human clicked in — a lane that jumps when an unrelated chip comes back on
 *  is disorienting. And turning the LAST category off is allowed: the view
 *  then says "every category is off" outright. The tempting alternative —
 *  snapping back to all-on — silently contradicts the chips, which is the
 *  "looks complete, isn't" failure this whole feature is shaped around. */
export function toggleCategory(
  active: readonly string[],
  category: string,
  order: readonly string[]
): string[] {
  const next = new Set(active);
  if (next.has(category)) next.delete(category);
  else next.add(category);
  const ordered = order.filter((c) => next.has(c));
  // Anything the caller's order doesn't mention keeps its relative position at
  // the end, rather than vanishing from the selection.
  for (const c of next) if (!order.includes(c)) ordered.push(c);
  return ordered;
}

/** How stale the gh half may get while following. Two orders of magnitude
 *  slower than the audit poll on purpose: `gh_activity` shells out to `gh`
 *  twice (network + process spawn), and issue/PR lifecycle events do not
 *  arrive on a 1.5-second cadence. */
export const GH_REFRESH_MS = 60_000;

/** Is a gh refresh due?
 *
 *  `null` means "never attempted", which is always due. A clock that jumped
 *  BACKWARDS (`nowMs < lastAttemptMs`) is also due: the arithmetic alternative
 *  freezes the gh layer until the clock catches up, which on a big jump is the
 *  rest of the session — the same "never let a clock make data silently
 *  disappear" rule the model applies to future-stamped events. */
export function shouldRefreshGh(lastAttemptMs: number | null, nowMs: number): boolean {
  if (lastAttemptMs === null || !Number.isFinite(lastAttemptMs)) return true;
  if (!Number.isFinite(nowMs)) return true;
  if (nowMs < lastAttemptMs) return true;
  return nowMs - lastAttemptMs >= GH_REFRESH_MS;
}

/** One sentence the view renders above the chart. Same shape as
 *  `timelinemodel`'s `CoverageNote` (mirrored, not imported — see the header). */
export interface TimelineNote {
  id: string;
  text: string;
}

/** Flatten an error into one line for a note. */
function errText(err: unknown): string {
  const raw =
    err instanceof Error ? err.message : typeof err === "string" ? err : err == null ? "" : String(err);
  const line = raw.replace(/\s+/g, " ").trim();
  if (line === "") return "no detail";
  return line.length > 160 ? line.slice(0, 160) + "…" : line;
}

/** What the view says when `gh_activity` failed.
 *
 *  The sentence has to name what is MISSING, not just that something went
 *  wrong: with the gh half absent, the chart still renders every audit event,
 *  and a chart with no PR dots is indistinguishable from a repo where nobody
 *  opened a PR. `gh_activity` fails whole rather than half-populating (Slice
 *  A), so "issue and PR points" is the exact loss, and the audit half is
 *  explicitly declared intact so the reader knows which parts to trust. */
export function ghUnavailableNote(err: unknown): TimelineNote {
  return {
    id: "gh-unavailable",
    text: `GitHub activity unavailable (${errText(err)}) — no issue or PR points are plotted in this window. Audit events are unaffected.`,
  };
}

/** ISO-8601 UTC, seconds precision — the same format `timelinemodel`'s own
 *  coverage notes use, so the two sets of sentences read as one voice. */
function isoUtc(ms: number): string {
  return new Date(ms).toISOString().replace(/\.\d{3}Z$/, "Z");
}

/** The precise coverage floor of a CAPPED gh list: the oldest `updated_at` in
 *  the page. Slice A pins the query to `sort:updated-desc` exactly so this is
 *  meaningful — the page is "the N most recently active", so nothing omitted
 *  has been active since its oldest row. Returns null when no row carries a
 *  parseable `updated_at` (then the view has only the vaguer cap note to
 *  give, and must not invent an instant). */
export function ghCoverageFloorMs(rows: readonly { updated_at?: string | null }[]): number | null {
  let oldest: number | null = null;
  for (const r of rows) {
    const raw = r.updated_at;
    if (typeof raw !== "string" || raw === "") continue;
    const ms = Date.parse(raw);
    if (!Number.isFinite(ms)) continue;
    if (oldest === null || ms < oldest) oldest = ms;
  }
  return oldest;
}

/** The sentence for that floor. Deliberately a POSITIVE statement of what IS
 *  covered ("complete back to T") plus the exact bound on what isn't — the
 *  design note's "state 'complete above T' rather than a vague 'may be
 *  incomplete'". `which` is the human word for the list ("issues" / "PRs"), so
 *  a truncated issue list never implies the PR list was truncated too. */
export function ghFloorNote(which: string, floorMs: number): TimelineNote {
  return {
    id: `gh-floor-${which}`,
    text: `GitHub ${which}: complete back to ${isoUtc(floorMs)} — nothing omitted by the cap has been active since then.`,
  };
}

/** Rows the click-to-expand detail body renders for one dot before it starts
 *  summarizing. A cluster can hold hundreds of events (that is what clustering
 *  is for); building a row per event would stall the pane. */
export const DETAIL_MAX_ROWS = 50;

/** Split a cluster's members into "rendered" and "counted".
 *
 *  `hidden` is returned rather than dropped so the view can print the
 *  remainder — a detail body that stops at 50 with no note is exactly the
 *  silent cap lessons.md names, and it is worse here than elsewhere because
 *  the dot beside it is labelled with the true count. */
export function detailSlice(indices: readonly number[]): { shown: number[]; hidden: number } {
  if (indices.length <= DETAIL_MAX_ROWS) return { shown: [...indices], hidden: 0 };
  return { shown: indices.slice(0, DETAIL_MAX_ROWS), hidden: indices.length - DETAIL_MAX_ROWS };
}
