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
  groupRows,
  matchesFilter,
  type AgentFilter,
  type AgentGroup,
  type AgentOrder,
  type AgentRow,
  type AgentState,
} from "./agentrows.ts";
import { agentMark, type AgentMarkView } from "./agenticons.ts";

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

/** What each group order is called on its control. `Record<AgentOrder, string>`
 *  is total, so an order added without a word for it fails to compile.
 *
 *  "most wants you" is the ladder's own phrasing (the badge counts exactly the
 *  states a person must act on) rather than "urgency", which would be a second
 *  vocabulary for one idea. */
export const AGENT_ORDER_LABEL: Record<AgentOrder, string> = {
  state: "most wants you",
  tab: "by tab",
};

/** The order the control offers its choices in — the object above IS the order,
 *  so there is no second list to forget an entry in. */
export const ORDER_CHOICES = Object.keys(AGENT_ORDER_LABEL) as AgentOrder[];

/** The groups to render: filtered, then grouped by tab, then ordered.
 *
 *  The filter is applied FIRST and that is what makes "a tab with no agent rows
 *  shows no header" true of a filtered list too — a tab whose every row was
 *  filtered out never reaches `groupRows` and so produces no group, exactly as
 *  a tab holding no panes does. Row order inside a group is `sortRows`',
 *  whichever chip and whichever order are selected. */
export function visibleGroups(
  rows: readonly AgentRow[],
  filter: AgentFilter,
  order: AgentOrder,
): AgentGroup[] {
  return groupRows(rows.filter((r) => matchesFilter(r, filter)), order);
}

/** The agent-type mark for a row (#2371), or `null` when there is nothing to
 *  draw.
 *
 *  ONE CALL, NO BRANCH. `row.harness` is the CLI loomux already knows this pane
 *  runs — `agentCli` off the launch line, or an SSH profile's declared far-end
 *  CLI — which is precisely `agentMark`'s `knownCli` input, so the resolver
 *  answers from the CLI's own name and a CLI added tomorrow shows up as itself.
 *  A `harness === "claude" ? … : …` here would be the #722/#841 defect: the
 *  fourth CLI silently inheriting the third one's badge.
 *
 *  `null` falls out of the resolver's own rule rather than being a case here: a
 *  row with no harness has no launch line to read, and `agentMark` answers
 *  `null` for that — "a plain shell is not an agent, and a row of `?` badges
 *  over every terminal is noise dressed as information".
 *
 *  RESIDUAL, stated because it is a real gap and not a rounding: `AgentRow`
 *  does not carry remoteness, so an SSH pane whose profile declares no
 *  `defaultCli` reads `harness: null` and draws nothing, where the pane HEADER
 *  draws the neutral "remote — agent CLI unknown" badge for the same pane. Both
 *  decline to name a CLI; the header is the surface that can afford to explain
 *  why, and the row's identity line already says the pane is what it is. */
export function agentRowMark(row: AgentRow): AgentMarkView | null {
  return agentMark({ knownCli: row.harness });
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
