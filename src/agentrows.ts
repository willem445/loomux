// The plain-data contract between a pane and the two views that summarise it:
// the Agents tab (#2122) and the pane Notes rows (#2116). DOM-free on purpose,
// so `test/agentrows.test.ts` builds `PaneFacts` literals and pins the state
// ladder without simulating a terminal (CLAUDE.md's "frontend logic that needs
// tests is extracted into DOM-free pure modules").
//
// WHY A PROJECTION AT ALL. Every fact below already exists on `Pane`, as a
// dozen scattered getters (`name`, `agentCli`, `orchGroupId`, `sessionId`,
// `attention`, `isDormant`, `isWelcome`, `tabPaneInfo()`, ...). A view that
// read them one by one would be coupled to `Pane`'s shape and untestable
// without a DOM; `Pane.facts()` hands over one frozen reading instead, and
// this module decides what it MEANS. The split is the point: `pane.ts` owns
// where the facts come from, this module owns what they add up to.

import { attentionPresentation, DECISION_REASONS } from "./attention.ts";
import { ACTIVITY_FLOOR_BYTES, type ActivitySnapshot } from "./paneactivity.ts";

/** One pane's whole reading, as plain data. Produced by `Pane.facts()`.
 *
 *  Every field is a projection of something the pane already knows — nothing
 *  here triggers IPC, reads geometry, or is unsafe on a hidden tab (the same
 *  contract `tabPaneInfo()` carries). */
export interface PaneFacts {
  /** Stable identity for THIS pane object, for the lifetime of this window.
   *  Minted in the `Pane` constructor from a module counter and never
   *  persisted — a view keys its rows on this. Deliberately not `ptyId`
   *  (changes on every respawn) and not the pane name (the human renames it),
   *  and deliberately not stable across a restart, because nothing here needs
   *  it to be and a persisted key would be a schema. */
  readonly key: string;
  /** The header name the human sees, renames included. */
  readonly name: string;
  /** The pane's classification, straight off `tabPaneInfo().kind`. */
  readonly kind: string;
  /** Which agent CLI is running, READ OFF THE LAUNCH LINE — `agentCli` (i.e.
   *  `sessionCliFromCommand`) for a local pane, the SSH profile's declared
   *  far-end CLI for a remote one, null for a plain shell or an unrecognised
   *  program. Never branched on a CLI name to produce a name (#722/#841): a
   *  fourth CLI must show up here as itself, not inherit an else-branch. */
  readonly harness: string | null;
  /** This pane's orchestration identity, or null for every pane that has none
   *  (a plain shell, a bare agent pane, an SSH pane — which can never carry
   *  one at all). */
  readonly orch: { readonly group: string; readonly agentId: string | null; readonly role: string | null } | null;
  /** The agent session id this pane has recorded, if any (#440). */
  readonly sessionId: string | null;
  /** This pane is functional — `tabPaneInfo().live`, which is the repo's one
   *  answer to that question. True for a running PTY, and also for a CONTENT
   *  pane (files, editor, git, workflow), which has no PTY by design and is
   *  live the moment it exists. False for a welcome form, a dormant
   *  placeholder, and a pane whose process has exited — the ladder tells those
   *  three apart on its own rungs, and only the last is a failure. */
  readonly alive: boolean;
  /** Showing a dormant restore placeholder (no PTY yet). */
  readonly dormant: boolean;
  /** Showing the welcome/setup form (no PTY yet). */
  readonly welcome: boolean;
  /** The backend's current attention reading, or null. `detail` is the free
   *  text the scan attached; the label/urgency mapping stays in
   *  `attention.ts`, which this module imports rather than re-listing. */
  readonly attention: { readonly reason: string; readonly detail: string | null } | null;
  /** The delivery-held reason (#246), or null when nothing is held. */
  readonly held: string | null;
  /** The activity reading at the moment `facts()` was called. */
  readonly activity: ActivitySnapshot;
}

/** What a pane is doing, as one word. The ladder below assigns exactly one. */
export type AgentState =
  | "dead"
  | "dormant"
  | "held"
  | "attention"
  | "question"
  | "turn-done"
  | "idle"
  | "working";

// Neither reason class is re-listed here, and that is the whole point:
// `attentionPresentation(reason).urgent` and `DECISION_REASONS` both come from
// `attention.ts`, so adding a reason there stays ONE edit. An earlier draft kept
// a hand-maintained copy of the decision set in this module, which meant a
// reason added over there would render a chip while silently missing the
// `question` rung and undercounting the badge (#2195 review, rev-std finding 2).
// `test/attention.test.ts` pins that every known reason is classified exactly
// once, so a new reason cannot default quietly into the wrong rung either.

/** Precedence for `sortRows` — most-wants-you first. Index in this array IS
 *  the ladder's own order, so a state added to `AgentState` without a rung
 *  here fails to compile (`Record<AgentState, number>` is total). */
const STATE_ORDER: Record<AgentState, number> = {
  attention: 0,
  question: 1,
  held: 2,
  "turn-done": 3,
  working: 4,
  idle: 5,
  dormant: 6,
  dead: 7,
};

/** Read a pane's facts as one state. A precedence ladder: the FIRST rung that
 *  decides wins, and each rung is a strictly more urgent claim than the one
 *  below it, so a pane carrying several signals at once is reported by the one
 *  that most needs a human.
 *
 *  Deliberately takes no clock. Everything time-dependent — whether the output
 *  window has lapsed, how many bytes are in it — is already resolved by
 *  `PaneActivity.snapshot(nowMs)` at the moment `facts()` was called, so a
 *  second `nowMs` here would be a parameter that decides nothing while reading
 *  as though it did. (The plan's sketch carried one; see
 *  `doc/design/agents-tab.md`.) */
export function deriveAgentState(facts: PaneFacts): AgentState {
  // 1. Dead: had a process, no longer has one, and is not a placeholder that
  //    never had one. A dead pane outranks a stale `waiting` sighting — the
  //    scan's last word about a process that has since exited is not news.
  if (!facts.alive && !facts.dormant && !facts.welcome) return "dead";
  // 2. Dormant: a restore placeholder. Nothing is running, by design.
  if (facts.dormant) return "dormant";
  // 3. Held: loomux is withholding a delivery to this pane (#246). Above
  //    attention because it is a state loomux ITSELF created and can explain.
  if (facts.held !== null) return "held";
  // 4/5. The backend's attention reading, split by urgency at `attention.ts`'s
  //    own line: urgent means wedged and it will not un-wedge itself.
  const reason = facts.attention?.reason ?? null;
  if (reason !== null && attentionPresentation(reason).urgent) return "attention";
  if (reason !== null && DECISION_REASONS.has(reason)) return "question";
  // 6. Turn done: either the scan says `waiting` right now, or it said so at
  //    some point and nothing has since disproved it (the latch — see
  //    `paneactivity.ts` for why the focus ack must not disprove it).
  if (reason === "waiting" || facts.activity.atPrompt) return "turn-done";
  // 7. Idle. TWO conditions, and the first is common to every pane kind on
  //    purpose (#2195 review B1). A pane painting above the floor is not idle,
  //    whatever else is true of it — hoisted out of the branches rather than
  //    repeated inside one of them, because a guard that reads a signal on one
  //    arm and not its sibling is a bypass exactly the width of that asymmetry
  //    (CLAUDE.md: a guard reads every one of its inputs by one rule). The
  //    first draft read it on the orch arm alone, and an unattended
  //    non-orchestration agent pane — `main.ts`'s resume-agent / fresh-agent /
  //    plain-session-restore all open one with a command and no orchGroup —
  //    therefore read `idle` for its entire working run.
  if (facts.activity.bytesInWindow < ACTIVITY_FLOOR_BYTES) {
    //  The SECOND condition is what differs, because the available evidence
    //  differs. An orchestration pane has the roster's own reading ("the reaper
    //  would call this idle"). A pane the roster does not cover has one fact
    //  left once the floor above has been applied: nobody has ever prompted it.
    const quietlyIdle =
      facts.orch !== null ? facts.activity.rosterIdle === true : facts.activity.lastHumanInputMs === null;
    if (quietlyIdle) return "idle";
  }
  // 8. Working is the DEFAULT, and the honest reading of it is "no evidence of
  //    a prompt" rather than "measured to be busy". The docs say so.
  return "working";
}

/** One row as the two views render it. `notes` is the count slot #2116 fills;
 *  null means "notes are not loaded / not applicable", which is a different
 *  claim from 0 and renders differently. */
export interface AgentRow {
  readonly key: string;
  readonly name: string;
  readonly harness: string | null;
  readonly group: string | null;
  readonly agentId: string | null;
  readonly role: string | null;
  readonly state: AgentState;
  readonly notes: number | null;
}

/** Project one pane's facts into a row. `notes` is supplied by the caller
 *  because the count lives in #2116's store, not on the pane. */
export function toAgentRow(facts: PaneFacts, notes: number | null = null): AgentRow {
  return {
    key: facts.key,
    name: facts.name,
    harness: facts.harness,
    group: facts.orch?.group ?? null,
    agentId: facts.orch?.agentId ?? null,
    role: facts.orch?.role ?? null,
    state: deriveAgentState(facts),
    notes,
  };
}

/** A filter chip's selection: one state, or everything. */
export type AgentFilter = "all" | AgentState;

/** Whether a row survives the current filter chip. */
export function matchesFilter(row: AgentRow, filter: AgentFilter): boolean {
  return filter === "all" || row.state === filter;
}

/** Rows in display order: most-wants-you state first, then by name so the
 *  order inside one state is stable as states change around it. Returns a new
 *  array — the caller's input is not mutated, so a view can hold its source
 *  list unsorted. */
export function sortRows(rows: readonly AgentRow[]): AgentRow[] {
  return [...rows].sort(
    (a, b) => STATE_ORDER[a.state] - STATE_ORDER[b.state] || a.name.localeCompare(b.name),
  );
}

/** How many rows are actually waiting on the human — the badge number. The two
 *  states that mean "a person must do something", and no others: `held` is
 *  loomux's own doing and clears itself, `turn-done` is a finished turn nobody
 *  is blocked on, and `dead`/`dormant` want nothing. */
export function needsYouCount(rows: readonly AgentRow[]): number {
  return rows.filter((r) => r.state === "attention" || r.state === "question").length;
}
