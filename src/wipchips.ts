/** Board WIP limit chips (#1175) — the `3/4` affordance kanban boards put on a
 *  column header, rendered on loomux's board header because loomux's board is a
 *  priority-ordered LIST and has no per-status columns to hang one on. When
 *  #1105 gives it columns, these chips move onto them: the shape a column
 *  header needs is exactly what `WipChip` already carries.
 *
 *  DOM-free on purpose (the repo's rule for frontend logic worth testing): the
 *  view renders what this returns and decides nothing.
 *
 *  **This module never counts anything.** `count` and `limit` both arrive from
 *  the backend, where `wip_occupants` is the single definition of what a cap
 *  counts (leaf rows only — a container's status is a rollup of the work its
 *  children carry). A tally here would be a second definition of the same rule,
 *  and the first time either side learned something about containers the board
 *  would be showing a number its own refusals disagree with. */

import type { WipCap } from "./orchestration.ts";
import { STATUSES } from "./taskboard.ts";

/** How full a cap is. Three states, not two: **full** is the state the practice
 *  is actually about (start nothing new), while **over** additionally means the
 *  board is somewhere a cap says it should not be — reachable by a human edit,
 *  by a cap lowered under a live board, or by warn mode doing exactly what warn
 *  mode is for. */
export type WipFill = "under" | "full" | "over";

/** One rendered cap. `text` is the chip's label, `title` its tooltip, and
 *  `fill` is what the view turns into a class — no styling decisions here. */
export interface WipChip {
  status: string;
  text: string;
  title: string;
  fill: WipFill;
  /** The repo declared `board.enforce: true`: an agent's write into this status
   *  while it is full is refused, not warned about. Never true of the human's
   *  own board edits, which is why the tooltip says whose writes it stops. */
  enforce: boolean;
}

const fillOf = (count: number, limit: number): WipFill =>
  count > limit ? "over" : count >= limit ? "full" : "under";

/** Board order (`STATUSES`), not the order the backend listed them in.
 *
 *  The backend's caps come out of a `BTreeMap`, so they arrive alphabetically —
 *  `in-progress, pr, prototype, queued, review` — which reads as a shuffle of a
 *  board whose whole meaning is its flow. A status the frontend does not know
 *  (a newer backend's ninth) sorts to the END rather than being dropped: an
 *  unknown cap is still a real limit the human is subject to, and hiding it
 *  would be the board lying by omission. */
const boardOrder = (status: string): number => {
  const i = (STATUSES as readonly string[]).indexOf(status);
  return i < 0 ? STATUSES.length : i;
};

function tooltip(c: WipCap, fill: WipFill): string {
  const head = `${c.status}: ${c.count} of a declared limit of ${c.limit}.`;
  if (fill === "under") {
    return `${head} Room for ${c.limit - c.count} more.`;
  }
  const over = fill === "over" ? ` It is over by ${c.count - c.limit}.` : "";
  const advice = ` Finish or re-status something in ${c.status} before starting more work.`;
  const enforced = c.enforce
    ? ` The orchestrator's own writes into ${c.status} are refused while it is full (board.enforce: true) — your edits here are not.`
    : ` orrerix warns the orchestrator about this; it does not refuse anything (set board.enforce: true in .loomux/workflow.yml to make it a refusal for agents).`;
  return head + over + advice + enforced;
}

/** The chips to render for a board, in board order. Empty for the repos — most
 *  of them — that declare no caps, which is what keeps the header exactly as it
 *  was before this feature existed. */
export function wipChips(caps: readonly WipCap[] | undefined): WipChip[] {
  if (!caps?.length) return [];
  return [...caps]
    .sort((a, b) => boardOrder(a.status) - boardOrder(b.status))
    .map((c) => {
      const fill = fillOf(c.count, c.limit);
      return {
        status: c.status,
        text: `${c.status} ${c.count}/${c.limit}`,
        title: tooltip(c, fill),
        fill,
        enforce: c.enforce,
      };
    });
}
