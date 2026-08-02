// Merge-queue visibility (#581 slice F) — DOM-free derivations over the
// `orch_merge_queue` payload (orchestration.ts) for the lifecycle chrome.
//
// Design note: doc/design/merge-queue.md. §4 defines the eight entry states,
// §11.3 the `merge_queue.json` schema the payload projects, §11.6 this surface.
// This module never fetches, never renders, and never decides anything about
// the queue: the queue is host-run (§3), and the frontend's whole role is to
// say, accurately, what it is doing.
//
// Three rules, each of which is a test in test/mergequeue.test.ts:
//
// 1. **Every state the core defines renders as itself.** All eight get a label
//    and a tone here; none renders blank, and none renders as "unknown".
// 2. **A ninth state fails LOUD.** `entryStateLabel` throws
//    `UnknownMergeQueueState` rather than inventing a word for it. A state this
//    build does not know can only mean the backend moved under an older
//    frontend, and a made-up label ("unknown", "—", or worse, a plausible
//    neighbour) is a confident wrong sentence in front of a human deciding
//    whether their PR is stuck. The caller catches it into a loud row; what it
//    must never do is degrade quietly.
// 3. **Truncation is surfaced, never silent** (the #608 convention). Both cuts
//    — the backend's wire cap and this module's row cap — end up in one honest
//    note that names how many of how many are shown.

import type { MergeQueueBatch, MergeQueueEntry, MergeQueueStatus } from "./orchestration";

/** The eight entry states (§4), in lifecycle order. There is no ninth, and
 *  this array is the only place that list is written down frontend-side. */
export const ENTRY_STATES = [
  "queued",
  "batching",
  "ci-wait",
  "landing",
  "bisecting",
  "landed",
  "kicked-back",
  "cancelled",
] as const;

export type EntryState = (typeof ENTRY_STATES)[number];

/** The four `status` words `orch_merge_queue` can report (mergeqview.rs). */
export const QUEUE_STATUSES = ["absent", "unreadable", "unsupported-version", "ok"] as const;

export type QueueStatus = (typeof QUEUE_STATUSES)[number];

/** How a row reads at a glance. Deliberately about the QUEUE's disposition,
 *  not about severity in general: `bad` means "this PR is not going to land as
 *  things stand", which is the one thing a human scanning the row needs. */
export type MergeQueueTone = "waiting" | "active" | "good" | "bad" | "muted";

/** Thrown for a state word outside [`ENTRY_STATES`] — see rule 2 at the top.
 *  A named error class so a caller can tell this apart from a fetch failure
 *  and say so, instead of showing a generic toast. */
export class UnknownMergeQueueState extends Error {
  readonly value: string;
  constructor(value: string) {
    super(
      `merge-queue state ${JSON.stringify(value)} is not one of the eight this build knows ` +
        `(${ENTRY_STATES.join(", ")}) — refusing to render a state it cannot explain`
    );
    this.name = "UnknownMergeQueueState";
    this.value = value;
  }
}

/** Thrown for a `status` word outside [`QUEUE_STATUSES`], for the same reason
 *  and with the same posture: a vocabulary this build does not know is a
 *  backend/frontend drift, not a data condition to paper over. */
export class UnknownMergeQueueStatus extends Error {
  readonly value: string;
  constructor(value: string) {
    super(
      `merge-queue status ${JSON.stringify(value)} is not one of ` +
        `(${QUEUE_STATUSES.join(", ")}) — refusing to render a payload it cannot interpret`
    );
    this.name = "UnknownMergeQueueStatus";
    this.value = value;
  }
}

const STATE_LABEL: Record<EntryState, string> = {
  queued: "queued",
  batching: "in the batch being built",
  "ci-wait": "waiting on batch CI",
  landing: "landing",
  bisecting: "in the bisect",
  landed: "landed",
  "kicked-back": "kicked back",
  cancelled: "cancelled",
};

const STATE_TONE: Record<EntryState, MergeQueueTone> = {
  queued: "waiting",
  batching: "active",
  "ci-wait": "active",
  landing: "active",
  // The batch went red and the search is running — this entry may be the
  // culprit. "active" would read as progress; it is progress toward blame.
  bisecting: "bad",
  landed: "good",
  "kicked-back": "bad",
  cancelled: "muted",
};

/** Whether `value` is one of the eight, as a type guard — for callers that
 *  want to branch rather than catch. */
export function isEntryState(value: string): value is EntryState {
  return (ENTRY_STATES as readonly string[]).includes(value);
}

/** The human label for a state. **Throws [`UnknownMergeQueueState`]** on a
 *  ninth — see rule 2. */
export function entryStateLabel(value: string): string {
  if (!isEntryState(value)) throw new UnknownMergeQueueState(value);
  return STATE_LABEL[value];
}

/** The tone for a state. Throws on a ninth, exactly like the label — a tone
 *  is a claim about the entry too, and a default one would be a guess. */
export function entryStateTone(value: string): MergeQueueTone {
  if (!isEntryState(value)) throw new UnknownMergeQueueState(value);
  return STATE_TONE[value];
}

/** One rendered entry. `text` is the whole line; `pr`/`tone` are for the
 *  chrome's link and CSS class. */
export interface MergeQueueRow {
  pr: number;
  /** The state's own label, without the PR number or the blocked clause. */
  label: string;
  tone: MergeQueueTone;
  /** §4's live eligibility predicate, verbatim from the backend — the only
   *  thing that explains a `queued` entry nothing is picking up. */
  blockedReason: string | null;
  text: string;
}

/** What the chrome should show. `hidden` is the ONLY silent case, and it is
 *  reserved for "there is no queue file at all" — never for a queue that
 *  exists and could not be read. */
export type MergeQueueView =
  | { kind: "hidden" }
  | { kind: "problem"; line: string; detail: string | null }
  | { kind: "queue"; line: string; rows: MergeQueueRow[]; note: string | null };

/** How many entry rows the chrome draws before the note takes over. The queue
 *  caps itself at 64 (§10) and the lifecycle panel is a few lines of chrome,
 *  not a table — so this cut is normal, which is exactly why it has to be
 *  stated rather than assumed. */
export const ROW_LIMIT = 6;

/** The tooltip for the row — one sentence on what this is and what it can't
 *  do, since "the host merges things" is a surprising sentence to meet in
 *  chrome. */
export const MERGE_QUEUE_HELP =
  "Read-only view of this group's merge queue (merge_queue.json). The queue lands batches on " +
  "an integration branch only — never the default branch — and every sub-PR still has to pass " +
  "the repo's own merge gate.";

function statusOf(status: MergeQueueStatus): QueueStatus {
  const s = status.status;
  if (!(QUEUE_STATUSES as readonly string[]).includes(s)) throw new UnknownMergeQueueStatus(s);
  return s as QueueStatus;
}

/** "#612 queued — blocked: head moved, verdicts stale" */
function rowFor(entry: MergeQueueEntry): MergeQueueRow {
  const label = entryStateLabel(entry.state);
  const blocked = entry.blocked_reason?.trim() ? entry.blocked_reason.trim() : null;
  return {
    pr: entry.pr,
    label,
    tone: blocked ? "bad" : entryStateTone(entry.state),
    blockedReason: blocked,
    text: blocked ? `#${entry.pr} ${label} — blocked: ${blocked}` : `#${entry.pr} ${label}`,
  };
}

/** "batch mq-7f3a (#640) waiting on batch CI · #612 + #613" — the batch is the
 *  thing a human actually watches, so it gets named in the header line rather
 *  than being one more row. */
export function batchLine(batch: MergeQueueBatch): string {
  const bits = [`batch ${batch.id}`];
  if (batch.draft_pr !== null) bits.push(`(#${batch.draft_pr})`);
  bits.push(entryStateLabel(batch.state));
  if (batch.prs.length) bits.push(`· ${batch.prs.map((p) => `#${p}`).join(" + ")}`);
  return bits.join(" ");
}

/** The truncation note (#608's convention), or `null` when nothing was cut.
 *
 *  Two independent cuts can have happened — the backend's wire cap
 *  (`entries_total` > `entries.length`, which it reports as `truncated`) and
 *  this module's `ROW_LIMIT`. They compose into one sentence, because a human
 *  needs one number pair, not an audit of who dropped what.
 *
 *  `truncated` is honored even when the arithmetic disagrees with it: a
 *  payload claiming a cut it cannot demonstrate still gets a `+`, because the
 *  reader that did the cutting is the authority on whether one happened, and
 *  the failure mode this convention exists to prevent is a short list reading
 *  as a whole one. */
export function truncationNote(shown: number, status: MergeQueueStatus): string | null {
  const total = Math.max(status.entries_total, status.entries.length, shown);
  if (shown >= total && !status.truncated) return null;
  const totalText = status.truncated && total <= shown ? `${total}+` : `${total}`;
  return `showing ${shown} of ${totalText} entries`;
}

/** Turn an `orch_merge_queue` payload into the chrome's view.
 *
 *  **Throws** `UnknownMergeQueueState` / `UnknownMergeQueueStatus` on a
 *  vocabulary this build does not know (rule 2) — the caller renders that as a
 *  loud row. Every other outcome is a rendered view; nothing here returns an
 *  empty queue to describe a queue it failed to read. */
export function mergeQueueView(status: MergeQueueStatus, rowLimit = ROW_LIMIT): MergeQueueView {
  switch (statusOf(status)) {
    case "absent":
      // The product default (§12): no file, no queue, no row. The one case
      // where saying nothing is the accurate thing to say.
      return { kind: "hidden" };
    case "unreadable":
      return {
        kind: "problem",
        line: "merge queue: merge_queue.json is unreadable — not showing a queue loomux can't read",
        detail: status.detail,
      };
    case "unsupported-version":
      return {
        kind: "problem",
        line: `merge queue: merge_queue.json is version ${status.version ?? "?"} — this build ` +
          `can't read it`,
        detail: status.detail,
      };
    case "ok":
      break;
  }

  const rows = status.entries.slice(0, Math.max(0, rowLimit)).map(rowFor);
  const bits = ["merge queue"];
  if (status.target.trim()) bits.push(`→ ${status.target.trim()}`);
  const total = Math.max(status.entries_total, status.entries.length);
  bits.push(total === 1 ? "1 entry" : `${total} entries`);
  if (status.batch) bits.push(batchLine(status.batch));
  return { kind: "queue", line: bits.join(" · "), rows, note: truncationNote(rows.length, status) };
}
