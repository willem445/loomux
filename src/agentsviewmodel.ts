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
import { agentMark, type AgentMarkInput, type AgentMarkView } from "./agenticons.ts";

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

/** One element the Agents list renders, in the order it renders them. `key` is
 *  the map key its element is held under — a tab id for a header, a pane key
 *  for a row — and the two kinds are keyed in SEPARATE maps, so a tab and a
 *  pane sharing a string cannot collide. */
export type ListSlot =
  | { readonly kind: "header"; readonly key: string; readonly title: string }
  | { readonly kind: "row"; readonly key: string; readonly row: AgentRow };

/** The exact sequence of elements `AgentsView.renderGroups` places, as data.
 *
 *  Extracted so the reconcile ORDER is testable at all (#2371 review round 2,
 *  premortem). `renderGroups` is DOM wiring, which this repo validates by hand,
 *  but the thing most likely to be wrong in it is not the DOM calls — it is
 *  *what order and under what keys*, and that is a pure projection. So the view
 *  keeps only the placement and the sweep, and the sequence is pinned here.
 *
 *  The headerless group contributes its rows and NO header: there is nothing to
 *  call it, and an invented "Other" would be a claim. That is why a header's
 *  key is read off `tab.id` inside the branch rather than from a sentinel — the
 *  map never holds an entry for a group that has no header. */
export function listSlots(groups: readonly AgentGroup[]): ListSlot[] {
  const slots: ListSlot[] = [];
  for (const group of groups) {
    if (group.tab !== null) slots.push({ kind: "header", key: group.tab.id, title: group.tab.title });
    for (const row of group.rows) slots.push({ kind: "row", key: row.key, row });
  }
  return slots;
}

/** The least a `sweep` entry has to be: something removable. Structural, not
 *  `HTMLElement`, so this module stays free of the DOM and the sweep below is
 *  testable with a two-line fake instead of a simulated document. */
export interface Removable {
  remove(): void;
}

/** Drop every entry the last render did not place, removing its element.
 *
 *  One helper for the header map and the row map, because it was the same six
 *  lines twice (#2371 review round 3, finding 2) — and because the two must
 *  stay the same: a sweep that ran on one map and not the other would leave
 *  orphaned elements behind exactly where the two lifetimes differ (a row
 *  survives its tab's header going away; a header survives all of its rows).
 *
 *  It lives HERE rather than in the view for the reason `listSlots` does: what
 *  it decides is pure, and the view is where this repo deliberately does not
 *  write tests. Leaving it in `agentsview.ts` left it invisible to the suite —
 *  measured, not assumed: disabling its body reddened nothing and `tsc` stayed
 *  silent.
 *
 *  Iterated over a COPY of the entries, since it deletes as it goes. */
export function sweep<T extends { el: Removable }>(held: Map<string, T>, seen: ReadonlySet<string>): void {
  for (const [key, entry] of [...held]) {
    if (seen.has(key)) continue;
    entry.el.remove();
    held.delete(key);
  }
}

/** The agent-type mark for a row (#2371), or `null` when there is nothing to
 *  draw.
 *
 *  ONE CALL, NO BRANCH, AND — the part that took a review round to get right —
 *  THE SAME INPUT THE PANE HEADER USES. `row.mark` is `Pane.agentMarkInput`
 *  carried through untouched, so the row and the header are two renderings of
 *  one resolution rather than two resolutions that agree by inspection. Every
 *  answer this function can give is `agentMark`'s own: the licensed mark, the
 *  letter badge, the neutral remote badge, or `null` for a pane with no launch
 *  line at all ("a plain shell is not an agent, and a row of `?` badges over
 *  every terminal is noise dressed as information").
 *
 *  IT USED TO READ `row.harness`, AND THAT WAS THE DEFECT (#2371 review round
 *  2, W1). `harness` is `sessionCliFromCommand`, a closed four-name membership
 *  test built for session-store adoption, so a local `codex`, `gemini`,
 *  `hermes` or `ante` pane — half of `AGENTS` — read `null` and drew NOTHING on
 *  its row while its own header drew `Agent CLI: codex`. It was the #722/#841
 *  outcome reached by a whitelist instead of a ternary, and widening the
 *  whitelist would have fixed four names while leaving the next CLI to
 *  rediscover it. Sharing the derivation is what makes "a CLI added tomorrow
 *  shows up as itself" true rather than merely claimed. */
export function agentRowMark(row: AgentRow): AgentMarkView | null {
  return agentMark(row.mark);
}

/** A comparable key for a mark input: equal exactly when `agentMark` would give
 *  the same answer, so a view can cache a painted mark against it.
 *
 *  IT EXISTS BECAUSE THE PAINT CACHE OUTLIVED ITS KEY (#2371 review round 3,
 *  finding 1). `AgentsView.updateRow` repaints the mark only when its cached
 *  reading changed, and that guard was still keyed on `harness` after round 2
 *  moved the mark's SOURCE to the wider `agentMarkInput`. `Pane.key` is
 *  `readonly` and survives `respawnFresh`, which rewrites `spawnCommand` and
 *  `spawnArgv` in place and repaints the HEADER — so promoting a `codex` pane
 *  to a `gemini` orchestrator (#407's door, `orchestration.ts` →
 *  `respawnFresh(promotePaneOptions(...))`) left `harness` null→null, the header
 *  showing the new badge and the row showing the old one indefinitely. That is
 *  W1's divergence again, surviving in a cache rather than in a derivation.
 *
 *  A STRING, not a field-by-field compare, because `argv` is an array: identity
 *  comparison would depend on whether a respawn happened to reuse the array,
 *  which is a fact about `pane.ts`'s internals rather than about the mark.
 *
 *  `undefined` and `null` deliberately collapse to the same key. Every field of
 *  `AgentMarkInput` is optional and `agentMark` reads them with `??`/truthiness,
 *  so the two are indistinguishable TO THE RESOLVER — and this key's contract is
 *  "equal when the answer would be equal", not "equal when the inputs are
 *  identical". A key that separated them would repaint for no visible change. */
export function markKey(mark: AgentMarkInput): string {
  return JSON.stringify([mark.command ?? null, mark.argv ?? null, mark.knownCli ?? null, mark.remote ?? false]);
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
