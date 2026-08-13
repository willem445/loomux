// Pure presentation mapping for the per-pane delivery-queue badge (#814): the
// backend's `orch-queue-depth` reading → the label/tooltip on the pane header
// and the marker on a minimized pane's dock chip. DOM-free (the split
// `heldbadge.ts` and `attention.ts` already use) so the wording, the age
// formatting and the stalled cue are unit-testable without a webview.
//
// **What it is for, and why the count is on screen rather than in a tooltip.**
// The queue is loomux's answer to "the pane is busy, hold the prompt" — nothing
// is lost, but until now nothing on screen said how much was waiting or since
// when, so a genuinely stuck pane and a briefly busy one looked identical
// (#814, filed after the stuck-prompt incident: what a human needed was "is
// this flowing?" at a glance). #813's own lesson is the second half of the
// brief: the stuck-prompt chip carried its detail on HOVER and the human never
// knew to hover. So the depth, the cap and the age are all in the label, and
// the tooltip only adds what a sentence can say and a chip cannot.
//
// Distinct from the other two header chips, and deliberately not merged with
// either: `.pane-attn` flags a pane the human should look AT, `.pane-held`
// flags that loomux is withholding ONE delivery right now, and this says how
// much has piled up behind it and for how long. A pane can wear all three, and
// each answers a different question.

/** One pane's live queue reading — the Rust `queue::QueueDepthItem`. */
export interface QueueDepthReading {
  pty_id: number;
  agent_id: string;
  depth: number;
  cap: number;
  /** Age of the oldest undelivered work, ms, already coarsened backend-side. */
  waiting_ms: number;
  /** Past the backend's `QUEUE_STALLED_AFTER` — not flowing. */
  stalled: boolean;
}

export interface QueuePresentation {
  /** Header-chip text. Carries the count, the cap and the age — never hover-only. */
  label: string;
  /** The sentence a hover adds; detail, never the sole surface. */
  title: string;
  /** Drives the chip's urgent styling (and the dock chip's). */
  stalled: boolean;
}

/** A wait as a human reads it: seconds under a minute, then minutes, then
 *  hours. Floors at every step — a badge must never claim a longer wait than
 *  has actually elapsed — and never renders a bare "0m" for a sub-minute wait,
 *  which would read as "no wait" on the pane that just started backing up. */
export function formatWaiting(ms: number): string {
  const safe = Number.isFinite(ms) && ms > 0 ? Math.floor(ms) : 0;
  const seconds = Math.floor(safe / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours}h` : `${hours}h${rest}m`;
}

/** Map a backend reading to the header chip's label and tooltip.
 *
 *  The tooltip says what a human should DO with each state rather than
 *  restating the numbers: a stalled queue names the two things that actually
 *  hold a pane (an unanswered question on screen, or the human's own
 *  unsubmitted text in the box), because those are the ones a human can clear;
 *  a full one says what loomux does next, which is drop arrivals — the same
 *  fact `queue::at_capacity_notice` tells the orchestrator, so the two channels
 *  cannot disagree.
 *
 *  Careful about "held": this reading is depth and age only — the backend never
 *  consults hold state to build it — so a pane whose senders simply outrun a
 *  perfectly healthy drainer reaches this badge with no hold of any kind. The
 *  hold is offered as the thing to CHECK, never asserted, which is the rule
 *  `queue::pressure_notice`'s doc settled for the same reason. */
export function queuePresentation(reading: QueueDepthReading): QueuePresentation {
  const { depth, cap, waiting_ms, stalled, agent_id } = reading;
  const age = formatWaiting(waiting_ms);
  const glyph = stalled ? "⚠" : "⇥";
  const label = `${glyph} ${depth}/${cap} queued · ${age}${stalled ? " stalled" : ""}`;
  const whose = agent_id ? `for ${agent_id}` : "for this pane";
  const full =
    depth >= cap
      ? ` The queue is FULL (${cap}/${cap}), so further deliveries to it are DROPPED, not queued.`
      : "";
  const state = stalled
    ? `Nothing has been delivered here for ${age}. Check the pane for a question waiting on an ` +
      `answer, or your own unsubmitted text in its input box — releasing either drains the backlog.`
    : `Oldest has been waiting ${age}. Deliveries are typed in one at a time, oldest first.`;
  return {
    label,
    title: `${depth} ${depth === 1 ? "delivery" : "deliveries"} queued ${whose}. ${state}${full}`,
    stalled,
  };
}

/** The dock-chip form (#814), for a pane that is minimized: its header — and so
 *  its queue chip — is out of the DOM entirely.
 *
 *  Not a nicety. Delegate agent panes open minimized by default
 *  (`spawn_opens_minimized`), so the panes whose queues back up the most are
 *  exactly the ones whose header nobody is looking at. Short by necessity — the
 *  chip is a name and two glyphs wide — with the full sentence on the title the
 *  grid already sets. `null` for a pane with an empty queue. */
export function dockChipQueue(reading: QueueDepthReading | null): { marker: string; stalled: boolean } | null {
  if (!reading || reading.depth <= 0) return null;
  return { marker: `${reading.stalled ? "⚠" : "⇥"}${reading.depth}`, stalled: reading.stalled };
}

/** Index a pushed set by pty, for the handler that has to apply it across every
 *  pane in every tab.
 *
 *  The backend pushes the FULL current set on every change, so a pane that is
 *  absent from it has an empty queue — which is why the handler clears by
 *  absence rather than waiting for a paired "cleared" event. Pure, so the
 *  lookup half of that decision is testable without a grid.
 *
 *  **A miss is `undefined`, and the call site's `?? null` is a contract, not
 *  tidying.** `setQueueDepth` takes `QueueDepthReading | null`, so `Map.get`'s
 *  `undefined` has to be converted — and because the project compiles under
 *  `strict`, dropping that `??` is a type error rather than a pane whose badge
 *  clears down the falsy branch by accident. This module's own test asserts the
 *  `undefined`, so the two agree about which value a miss actually is. */
export function readingsByPty(items: QueueDepthReading[]): Map<number, QueueDepthReading> {
  const byPty = new Map<number, QueueDepthReading>();
  // Last wins, and it cannot happen: the backend keys its snapshot by pty. A
  // duplicate would mean two readings for one pane, and silently keeping both
  // (rendering whichever the iteration reached last) is worse than picking one
  // deterministically.
  for (const item of items) byPty.set(item.pty_id, item);
  return byPty;
}
