// Lock-resource chrome for the group view (#858): turns the backend's
// `orch_lock_state` payload into the rows the panel draws.
//
// DOM-free and Tauri-free on purpose, like `watchline.ts` and `mergequeue.ts`
// — everything worth testing lives here, and `groupview.ts` only appends
// elements. It also defines its own minimal input shape rather than importing
// from `orchestration.ts`, so this module has no dependency on the IPC layer
// (and no import cycle).
//
// Tone follows the existing convention: neutral for "this is just the state",
// amber for "somebody is waiting", bad for "loomux is about to take this back".

/** Shape this module needs from `orch_lock_state` — nothing more. */
export interface LockHolderLike {
  agent: string;
  note: string;
  acquired_ms: number;
  expires_ms: number;
}

export interface LockWaiterLike {
  agent: string;
  note: string;
  queued_ms: number;
  expires_ms: number;
}

export interface LockResourceLike {
  name: string;
  slots: number;
  max_hold_minutes: number;
  holders: LockHolderLike[];
  queue: LockWaiterLike[];
}

export type LockTone = "idle" | "held" | "waiting" | "urgent";

export interface LockRow {
  /** `build 1/1 · w-3 (cargo test) 12m · 2 waiting` */
  text: string;
  tone: LockTone;
  /** Multi-line hover detail: every holder and every waiter, in queue order. */
  detail: string;
}

/** Under this many minutes left on a hold, the row goes `urgent` — loomux is
 *  about to reclaim it, and that is the moment a human might want to look. */
export const URGENT_MINUTES = 5;

/** Whole minutes, rounded UP and never 0 for a live deadline — the
 *  `watchline.ts` rule: "0 min" reads as expired when it isn't. A deadline
 *  already in the past reads as 0, which is honest. */
export function minutesLeft(expiresMs: number, nowMs: number): number {
    return expiresMs <= nowMs ? 0 : Math.ceil((expiresMs - nowMs) / 60_000);
}

/** How long something has been going on: `45s`, `12m`, `2h 5m`. */
export function span(ms: number): string {
  const totalMin = Math.floor(Math.max(0, ms) / 60_000);
  if (totalMin === 0) return `${Math.floor(Math.max(0, ms) / 1000)}s`;
  if (totalMin < 60) return `${totalMin}m`;
  return `${Math.floor(totalMin / 60)}h ${totalMin % 60}m`;
}

function holderText(h: LockHolderLike, nowMs: number): string {
  const note = h.note ? ` (${h.note})` : "";
  return `${h.agent}${note} ${span(nowMs - h.acquired_ms)}`;
}

/** One row per declared resource, in the order the backend listed them. */
export function lockRows(resources: LockResourceLike[], nowMs: number): LockRow[] {
  return resources.map((r) => {
    const held = r.holders.length;
    const waiting = r.queue.length;
    const parts = [`${r.name} ${held}/${r.slots}`];
    if (held > 0) parts.push(r.holders.map((h) => holderText(h, nowMs)).join(", "));
    else parts.push("free");
    if (waiting > 0) parts.push(`${waiting} waiting`);

    const soonest = r.holders.length
      ? Math.min(...r.holders.map((h) => minutesLeft(h.expires_ms, nowMs)))
      : Number.POSITIVE_INFINITY;
    let tone: LockTone = "idle";
    if (held > 0) tone = "held";
    // Waiting outranks merely-held: a queue is the thing worth noticing.
    if (waiting > 0) tone = "waiting";
    // …and an imminent reclaim outranks both, because it is about to change
    // the state on its own.
    if (soonest <= URGENT_MINUTES) tone = "urgent";

    const detailLines: string[] = [];
    for (const h of r.holders) {
      detailLines.push(
        `holding: ${holderText(h, nowMs)} — reclaimed in ${minutesLeft(h.expires_ms, nowMs)} min`
      );
    }
    for (const [i, w] of r.queue.entries()) {
      const note = w.note ? ` (${w.note})` : "";
      detailLines.push(
        `#${i + 1} in queue: ${w.agent}${note} — waiting ${span(nowMs - w.queued_ms)}, gives up in ${minutesLeft(w.expires_ms, nowMs)} min`
      );
    }
    if (detailLines.length === 0) detailLines.push("nobody holds or wants this right now");
    return {
      text: parts.join(" · "),
      tone,
      detail: `${r.name}: ${r.slots} slot(s), max hold ${r.max_hold_minutes} min\n${detailLines.join("\n")}`,
    };
  });
}

/** The section's header line. Empty string when the repo declares no
 *  resources, so the caller hides the whole row rather than drawing an empty
 *  heading — the `watchLine` convention. */
export function lockSummary(resources: LockResourceLike[]): string {
  if (resources.length === 0) return "";
  const waiting = resources.reduce((n, r) => n + r.queue.length, 0);
  const held = resources.reduce((n, r) => n + r.holders.length, 0);
  const tail = waiting > 0 ? `, ${waiting} agent${waiting === 1 ? "" : "s"} queued` : "";
  return `locks: ${held} held across ${resources.length} resource${resources.length === 1 ? "" : "s"}${tail}`;
}
