// The Agents tab's presentation decisions (#2122 slice B), DOM-free so they can
// be tested against literals: what each state is CALLED, which filter chips
// exist and what they count, which rows survive the current chip, and what the
// identity line under a row's name reads.
//
// THE SPLIT. `agentrows.ts` (slice A) owns what a pane's facts MEAN — the
// ladder, the row projection, the sort order, the badge count. This module owns
// how that reading is presented, and `agentsview.ts` owns the elements. Nothing
// here re-decides a state or re-lists a reason: `sortRows`, `matchesFilter` and
// the `AgentState` union all come from slice A, so a rung added there flows
// through this module by the type system rather than by someone remembering.

import {
  matchesFilter,
  sortRows,
  type AgentFilter,
  type AgentRow,
  type AgentState,
} from "./agentrows.ts";

/** What each state is called in the UI. `Record<AgentState, string>` is TOTAL,
 *  so a rung added to the ladder without a word for it fails to compile rather
 *  than rendering a blank chip.
 *
 *  The words are the issue's own vocabulary, not a synonym set: `turn-done`
 *  reads "turn done" because "ready" or "waiting" would each claim something
 *  the latch does not measure (see the per-harness trust table in
 *  `doc/design/agents-tab.md`), and `working` reads "working" while honestly
 *  meaning "no evidence of a prompt". */
export const AGENT_STATE_LABEL: Record<AgentState, string> = {
  attention: "needs you",
  question: "question",
  // #2367: the header chip words the `report` reason "✓ reported", so the row
  // uses the same word — it used to read "question", which counted a
  // report-waiting-on-the-orchestrator as a decision owed by the human.
  reported: "reported",
  held: "held",
  "turn-done": "turn done",
  working: "working",
  idle: "idle",
  dormant: "dormant",
  dead: "exited",
};

/** The order chips are offered in: the ladder's own precedence, so the chip a
 *  human reaches for first is the one for the state that most wants them.
 *  Derived from `AGENT_STATE_LABEL`'s key order rather than re-listed — the
 *  object literal above IS the order, and a second array would be a second
 *  place to forget a state. */
const CHIP_ORDER = Object.keys(AGENT_STATE_LABEL) as AgentState[];

/** One filter chip. `count` is how many rows carry that state right now — of
 *  ALL rows, not of the visible ones, so switching chips does not change what
 *  the other chips claim. */
export interface FilterChip {
  readonly filter: AgentFilter;
  readonly label: string;
  readonly count: number;
  readonly selected: boolean;
}

/** The chips to render for `rows`, with `selected` marked.
 *
 *  `all` is always offered. A per-state chip is offered when it has rows OR
 *  when it is the one currently selected — the second half is not a nicety: a
 *  chip that vanished as its last row resolved would leave the human looking at
 *  an empty list with no control to clear the filter they are standing on. */
export function filterChips(rows: readonly AgentRow[], selected: AgentFilter): FilterChip[] {
  const counts = new Map<AgentState, number>();
  for (const r of rows) counts.set(r.state, (counts.get(r.state) ?? 0) + 1);
  const chips: FilterChip[] = [
    { filter: "all", label: "all", count: rows.length, selected: selected === "all" },
  ];
  for (const state of CHIP_ORDER) {
    const count = counts.get(state) ?? 0;
    if (count === 0 && selected !== state) continue;
    chips.push({
      filter: state,
      label: AGENT_STATE_LABEL[state],
      count,
      selected: selected === state,
    });
  }
  return chips;
}

/** The rows to render, filtered then ordered. The order is `sortRows`' — state
 *  urgency, then name — and it is applied AFTER the filter so it is the same
 *  order whichever chip is selected. */
export function visibleRows(rows: readonly AgentRow[], filter: AgentFilter): AgentRow[] {
  return sortRows(rows.filter((r) => matchesFilter(r, filter)));
}

/** The quiet line under a row's name: which CLI, which role/block, which group
 *  — whichever of those this pane actually has.
 *
 *  Absent parts are OMITTED rather than placeheld, so a plain shell reads as a
 *  plain shell instead of "— · — · —", and the separator never leads or
 *  trails. `role` is printed VERBATIM: for a workflow group it is a declared
 *  block id (`rev-security`, #222), and mapping it through a table of known
 *  role names is the #722/#841 defect — a fourth thing showing up wearing a
 *  third thing's name. */
export function agentIdentityLine(row: AgentRow): string {
  return [row.harness, row.role, row.group].filter((p): p is string => p !== null && p !== "").join(" · ");
}
