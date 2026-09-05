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

import { LAUNCHABLE_AGENT_PROGRAMS } from "./agents.ts";
import { markProgram, namesAnAgent, type AgentMarkInput } from "./agenticons.ts";
import { normalizeAgentProgram } from "./panerestore.ts";
import { attentionPresentation, DECISION_REASONS, REPORT_REASONS } from "./attention.ts";
import { ACTIVITY_FLOOR_BYTES, type ActivitySnapshot } from "./paneactivity.ts";

/** Which tab (workspace) a pane lives in, as the Agents tab groups by (#2371).
 *
 *  SUPPLIED BY THE CALLER, not derived by the pane, and that is a fact about
 *  the object graph rather than a shortcut: `Workspace` owns a `Grid` and a
 *  `Grid` owns its panes, with no back-reference in the other direction, so a
 *  `Pane` genuinely does not know which tab it is in. The one caller that needs
 *  the answer — the Agents view's `facts()` dep — reaches every pane BY walking
 *  `tabs.tabs`, so it is holding the tab at the moment it asks. Passing it in
 *  keeps `facts()`'s contract intact (no geometry, no IPC, no timer); a lookup
 *  would have to walk the whole tab set once per pane to recover something the
 *  caller already had. */
export interface TabRef {
  /** The workspace id. Stable for the tab's life and the key a group is
   *  identified by — never a path (it reaches no `.join`; hard constraint 6 is
   *  about group ids and is untouched by this type). */
  readonly id: string;
  /** The tab's human-facing name, renames included — what a group header
   *  reads. Not the group key: the human may name two tabs the same thing, and
   *  a rename must re-label a group rather than split or merge it. */
  readonly title: string;
  /** This tab's 0-based position in the tab strip when the reading was taken.
   *
   *  THE ORDER `groupRows` SORTS GROUPS BY under `"tab"`, and it is the strip's
   *  own order rather than the title's on purpose: the human arranges the strip
   *  by dragging (`dropTargetIndex`, #379), so that arrangement is a decision
   *  they made, while alphabetical order would silently reshuffle the whole
   *  list on a rename. */
  readonly index: number;
}

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
  /** The tab this reading was taken from, or `null` when the caller named
   *  none — see `TabRef`. `null` is a complete answer for every caller but the
   *  Agents view: the pane Notes rows (#2116) and `main.ts`'s focus walk group
   *  nothing, so neither has anything to name. */
  readonly tab: TabRef | null;
  /** Which agent CLI is running, as far as the SESSION STORE is concerned —
   *  `agentCli` (i.e. `sessionCliFromCommand`) for a local pane, the SSH
   *  profile's declared far-end CLI for a remote one, null otherwise. Never
   *  branched on a CLI name to produce a name (#722/#841): a fourth CLI must
   *  show up here as itself, not inherit an else-branch.
   *
   *  **Its `null` is NARROWER than "no agent is running here", and that is by
   *  design rather than a gap.** `sessionCliFromCommand` answers only the four
   *  CLIs loomux can scan sessions for, because its answer is matched against
   *  `listSessions()` rows; a `codex` or `gemini` pane is a real agent pane
   *  that no session store covers, so it reads `null` here and correctly so.
   *
   *  It therefore must NOT be used to decide what a pane IS — only what can be
   *  adopted for it. `mark` below is the field that answers "which program is
   *  running", and reading this one instead is what #2371 review round 2 W1
   *  found: four of the eight launchable CLIs drew no icon on their row while
   *  their pane header drew one. This is the identity line's field
   *  (`agentIdentityLine`) and `notesApplyToPane`'s; it is not the icon's. */
  readonly harness: string | null;
  /** Everything `agenticons.ts` may know about this pane, straight off
   *  `Pane.agentMarkInput` — the SAME object the pane header resolves its own
   *  mark from, so the two surfaces cannot answer differently about one pane
   *  (#2371 review round 2, W1).
   *
   *  Carried as the resolver's INPUT rather than as a resolved view, because a
   *  view is markup: `facts()` is called once a second per open pane and must
   *  stay a projection of state the pane already holds, so the SVG is built by
   *  whoever actually draws it, at the size they draw it. */
  readonly mark: AgentMarkInput;
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
  | "reported"
  | "turn-done"
  | "idle"
  | "working";

// Neither reason class is re-listed here, and that is the whole point:
// `attentionPresentation(reason).urgent`, `DECISION_REASONS` and `REPORT_REASONS`
// all come from `attention.ts`, so adding a reason there stays ONE edit. An
// earlier draft kept a hand-maintained copy of the decision set in this module,
// which meant a reason added over there would render a chip while silently
// missing the `question` rung and undercounting the badge (#2195 review,
// rev-std finding 2). `test/attention.test.ts` pins that every known reason is
// classified exactly once, so a new reason cannot default quietly into the
// wrong rung either.

/** Precedence for `sortRows` — most-wants-you first. Index in this array IS
 *  the ladder's own order, so a state added to `AgentState` without a rung
 *  here fails to compile (`Record<AgentState, number>` is total). */
const STATE_ORDER: Record<AgentState, number> = {
  attention: 0,
  question: 1,
  reported: 2,
  held: 3,
  "turn-done": 4,
  working: 5,
  idle: 6,
  dormant: 7,
  dead: 8,
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
  // 4/5/6. The backend's attention reading, split by class at `attention.ts`'s
  //    own line: urgent means wedged and it will not un-wedge itself; a decision
  //    waits on the human's own pace; a report (#2367) waits on the ORCHESTRATOR
  //    — nothing is owed by the human, so it takes its own rung below question.
  const reason = facts.attention?.reason ?? null;
  if (reason !== null && attentionPresentation(reason).urgent) return "attention";
  if (reason !== null && DECISION_REASONS.has(reason)) return "question";
  if (reason !== null && REPORT_REASONS.has(reason)) return "reported";
  // 7. Turn done: either the scan says `waiting` right now, or it said so at
  //    some point and nothing has since disproved it (the latch — see
  //    `paneactivity.ts` for why the focus ack must not disprove it).
  if (reason === "waiting" || facts.activity.atPrompt) return "turn-done";
  // 8. Idle. TWO conditions, and the first is common to every pane kind on
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
  // 9. Working is the DEFAULT, and the honest reading of it is "no evidence of
  //    a prompt" rather than "measured to be busy". The docs say so.
  return "working";
}

/** Is this pane an AGENT pane at all? The Agents tab's MEMBERSHIP rule, and
 *  the one place it is decided (#2514).
 *
 *  NOT `Pane.isAgentPane`, which shares the name and answers a different
 *  question — "was this pane launched with a command", true for a hand-typed
 *  `make` pane, which this is false for. That one gates the adopt-on-connect
 *  gesture; this one decides the list. Neither is a substitute for the other
 *  (#2514 review round 1, finding 1).
 *
 *  The ladder below has no rung for this, by design: `deriveAgentState`
 *  answers "what is this pane doing" and its default rung is `working` —
 *  honestly read as "no evidence of a prompt". Asked about a shell the human
 *  has typed into, it therefore says `working`, correctly, about a question it
 *  was never the right one to ask. Membership is a separate question and gets a
 *  separate function (#2514).
 *
 *  THREE arms, and the second and third differ in WHO IS CLAIMING:
 *
 *  1. `orch` — an orchestration pane is an agent whatever it was launched
 *     with, and a manager pane can carry no harness at all.
 *  2. `harness` on a remote pane is a far-end CLI a HUMAN declared, and it is
 *     held to the BADGE's answer — `declaresAnAgent`, below.
 *  3. the launch line is loomux's own INFERENCE, and it is held to the
 *     LAUNCHER'S OWN CATALOG — `namesLaunchableCli`, below.
 *
 *  ARM 3 IS NOT OPTIONAL AND ARM 2 CANNOT STAND IN FOR IT. `harness` is
 *  `sessionCliFromCommand`, a closed FOUR-name membership test built to match
 *  `listSessions()` rows — it answers `null` for `codex`, `gemini`,
 *  `hermes` and `ante`, which are four of the eight CLIs the launcher can
 *  start a pane on. A predicate resting on it alone would drop half the
 *  launchable agents out of the Agents tab AND out of `needsYouCount` — an
 *  agent asking the human a question, invisible. That is #2371 review round 2's
 *  W1 one layer down (`PaneFacts.harness` says so in its own doc: it must not
 *  be used to decide what a pane IS), and it is why this reads `mark` — the
 *  pane header's own input — through `markProgram`.
 *
 *  AND THE CATALOG IS NOT "ANY PROGRAM AT ALL". `agentMarkFor` is TOTAL: it
 *  gives a hand-typed `make` pane a lettered badge, because a badge is a
 *  fallback and membership is a claim. So a custom-command pane whose program
 *  loomux does not recognise is NOT a row — the same answer the human gets from
 *  the launcher, which offers exactly these eight. */
export function isAgentPane(facts: PaneFacts): boolean {
  if (facts.orch !== null) return true;
  if (declaresAnAgent(facts.harness)) return true;
  return namesLaunchableCli(markProgram(facts.mark));
}

/** A name a HUMAN declared for this pane — an SSH profile's far-end CLI, which is what
 *  `harness` carries on a remote pane — tested against the BADGE's own answer.
 *
 *  Deliberately NOT the launcher's catalog, and the two arms differ on purpose (#2514
 *  review round 3, B1). A launch line is loomux's own INFERENCE about a local process,
 *  so it is held to the eight CLIs loomux offers. A declared far-end CLI is a human's
 *  ASSERTION about a machine loomux cannot see, and the product supports asserting one
 *  the catalog does not name: `setSshCli` round-trips such a value, renders it as
 *  "<cli> — not a CLI orrerix knows", and warns rather than refusing. Holding that
 *  assertion to the catalog listed a pane's header as "Agent CLI: aider" while the tab
 *  had no row for it — the same header-vs-row divergence as the bug this arm was
 *  narrowed to fix, pointing the other way.
 *
 *  So the test is exactly the badge's: `namesAnAgent`, which is `agentMarkFor`'s own
 *  unknown-tier decision exported rather than a second denylist here. A profile
 *  declaring `bash` is still refused — the header says "a transport or shell, not an
 *  agent" and the row now agrees with it — and one declaring `aider` is listed, because
 *  the header says "Agent CLI: aider" and the row agrees with that too. One pane, one
 *  answer, whichever way it goes.
 *
 *  Normalized first, because `harness` is NOT pre-normalized the way `markProgram`'s
 *  answer is: a profile declaring `Claude.exe` is the same claim as one declaring
 *  `claude`. */
function declaresAnAgent(name: string | null): boolean {
  if (name === null) return false;
  const program = normalizeAgentProgram(name);
  return program !== "" && namesAnAgent(program);
}

/** The catalog test, applied to the name loomux INFERRED from the launch line.
 *
 *  `program` is `markProgram`'s answer and nothing else, so it is ALREADY
 *  normalized on every path — that function's three arms return
 *  `normalizeAgentProgram`'s output, `null`, and `programFromRestore`'s
 *  output, which normalizes too. There is deliberately no second
 *  `normalizeAgentProgram` here: it could never change its input, and a
 *  normalization that cannot change its input is a claim that some caller
 *  might pass a raw name, which is false. Round 2 needed one, because
 *  `harness` came through this function then; round 3 moved that to
 *  `declaresAnAgent`, which normalizes because its input really is raw. The
 *  mutation matrix is what found the leftover — deleting the call reddened
 *  nothing, which is the signature of a guard guarding a case that no longer
 *  reaches it.
 *
 *  The CATALOG rather than the resolver, and that half is load-bearing:
 *  `agentMarkFor` is total, so a hand-typed `make` pane gets a lettered
 *  badge, and "does this resolve to a program at all" would have listed it.
 *  An inference is held to the eight CLIs loomux itself offers. */
function namesLaunchableCli(program: string | null): boolean {
  return program !== null && LAUNCHABLE_AGENT_PROGRAMS.has(program);
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
  /** The tab this row's pane lives in (#2371), or null when the reading named
   *  no tab. Carried through unchanged from `PaneFacts.tab`. */
  readonly tab: TabRef | null;
  /** What the row's agent mark is resolved from — carried through unchanged
   *  from `PaneFacts.mark`, which is the pane header's own input. NOT
   *  `harness`: see that field for the divergence reading it caused. */
  readonly mark: AgentMarkInput;
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
    tab: facts.tab,
    mark: facts.mark,
  };
}

/** Every AGENT row in one window-wide reading: the membership rule and the
 *  projection, in one call.
 *
 *  One call rather than a `filter` the caller writes, because the rule has to
 *  hold for the RENDERED list and for the BADGE alike — "one rule, not two"
 *  (#2514) — and a caller that can reach `toAgentRow` directly is a second
 *  place the filter can be forgotten. `test/agentrows.test.ts` default-denies
 *  exactly that: `toAgentRow` has no caller in `src/` outside this module.
 *
 *  `notes` is not threaded through: the Agents tab passes none today, and a
 *  parameter no caller supplies is a claim about a caller that does not
 *  exist. `toAgentRow` still takes one for the caller that will. */
export function agentRows(facts: readonly PaneFacts[]): AgentRow[] {
  return facts.filter(isAgentPane).map((f) => toAgentRow(f));
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

/** Which order the human has chosen for the GROUPS (#2371).
 *
 *  Not for the rows: `sortRows` orders those inside every group either way, so
 *  this never changes what "most wants you" means within one tab. The choice is
 *  only ever about which tab's block you read first. */
export type AgentOrder = "state" | "tab";

/** The default when nothing is stored, and the pre-#2371 reading: most-wants-you
 *  first. A viewer who has never touched the control gets the order the tab
 *  already had. */
export const DEFAULT_AGENT_ORDER: AgentOrder = "state";

/** One tab's block of rows. `tab` is `null` only for rows whose reading named
 *  no tab, which the view renders with no header — there is nothing to call it. */
export interface AgentGroup {
  readonly tab: TabRef | null;
  readonly rows: readonly AgentRow[];
}

/** The strip position an unattributed group stands in for: past every real tab.
 *
 *  It decides the `"tab"` order outright — a group with no tab has no position
 *  in the strip, so it goes last — and under `"state"` it is only the TIE-BREAK.
 *  That asymmetry is deliberate: `"state"` exists to put the rows that most want
 *  you at the top, and a headerless group holding a wedged pane is still a
 *  wedged pane. Sorting it below a tab whose worst row is `idle` would hide
 *  urgency for the sake of tidiness, which is the opposite of what the order is
 *  for. */
const NO_TAB_INDEX = Number.MAX_SAFE_INTEGER;

/** Project rows into per-tab groups, ordered by the human's choice.
 *
 *  GROUPING IS UNCONDITIONAL and `order` decides only which group comes first.
 *  That is the shape the issue asks for: the headers are what make the fleet
 *  read by where it lives, so they are not something you have to switch an
 *  order to see, and it gives `"state"` a defined group order instead of an
 *  accidental one.
 *
 *   - `"state"` — the group holding the most urgent row first. Rows are already
 *     `sortRows`-ordered inside each group, so `rows[0]` IS that row and the
 *     comparison is one lookup rather than a second scan. Ties fall to strip
 *     order, so two tabs whose worst row is the same state stay in the human's
 *     own arrangement rather than in `Map` insertion order.
 *   - `"tab"` — strip order outright, `TabRef.index`. Deliberately NOT the
 *     title: see `TabRef.index`.
 *
 *  A TAB WITH NO ROWS PRODUCES NO GROUP, by construction rather than by a
 *  filter — the buckets are built from the rows, so a tab this call never saw
 *  cannot appear. That also means a filtered list (`matchesFilter` upstream)
 *  drops the header of a tab whose rows all filtered out, which is the same
 *  rule read from the other end.
 *
 *  Returns new arrays throughout; the caller's input is not mutated. */
export function groupRows(rows: readonly AgentRow[], order: AgentOrder): AgentGroup[] {
  // Keyed by tab id, NOT by title: two tabs may legally share a name, and a
  // rename must re-label one group rather than split or merge two. `Map` so a
  // tab id can never collide with `Object.prototype` and so iteration is
  // insertion order — which is the tie-break's starting point before it is
  // re-sorted below.
  const buckets = new Map<string, { tab: TabRef | null; rows: AgentRow[] }>();
  for (const row of rows) {
    const key = row.tab?.id ?? "";
    const found = buckets.get(key);
    if (found) {
      found.rows.push(row);
      // The LAST reading IN INPUT ORDER wins the label — not the "freshest",
      // which is not a thing a `TabRef` carries (#2371 review round 2, R2).
      // Every row of one tab carries the same `TabRef` in practice (one walk,
      // one title read), so this decides a case production does not produce;
      // it is stated rather than left implicit so the tie is a rule instead of
      // bucket-insertion luck.
      if (row.tab !== null) found.tab = row.tab;
    } else {
      buckets.set(key, { tab: row.tab, rows: [row] });
    }
  }
  const groups = [...buckets.values()].map((b) => ({ tab: b.tab, rows: sortRows(b.rows) }));
  const strip = (g: AgentGroup): number => g.tab?.index ?? NO_TAB_INDEX;
  // `rows` is never empty — a bucket exists only because a row created it — so
  // `rows[0]` is a real row and the `state` arm needs no absent-group case.
  const worst = (g: AgentGroup): number => STATE_ORDER[g.rows[0].state];
  groups.sort(
    order === "tab"
      ? (a, b) => strip(a) - strip(b)
      : (a, b) => worst(a) - worst(b) || strip(a) - strip(b),
  );
  return groups;
}

/** How many rows are actually waiting on the human — the badge number. The two
 *  states that mean "a person must do something", and no others: `held` is
 *  loomux's own doing and clears itself, `turn-done` is a finished turn nobody
 *  is blocked on, and `reported` (#2367) is waiting on the ORCHESTRATOR, not on
 *  the human — a report never raises the badge. `dead`/`dormant` want nothing. */
export function needsYouCount(rows: readonly AgentRow[]): number {
  return rows.filter((r) => r.state === "attention" || r.state === "question").length;
}

/** The pane key of the lead pane this pane reports to, or null when it nests
 *  under nobody (#2519 C1) — the Agents tab's child-indent input (C2 renders
 *  it; this module only decides what it MEANS, like every projection here).
 *
 *  The relationship is (same group, same tab), and BOTH halves are load-
 *  bearing. A lead's children join the lead's group (`spawn_agent` from a lead
 *  places them in it) and open in the lead's tab (C2 registers the tab as the
 *  group's owner), so:
 *
 *  - group alone is not enough — `groupRows` already scopes everything to one
 *    tab block, and an indent rendered from a cross-tab match would point at a
 *    parent that is not on screen;
 *  - tab alone is not enough — a worker of an ORCHESTRATOR group can share a
 *    tab with an unrelated lead, and it nests under nobody.
 *
 *  Only `worker`-class panes can be children: the backend refuses a lead every
 *  other kind, and a lead (role `"lead"`), an orchestrator, a manager or a
 *  plain pane resolves to null even when the group/tab pair matches its own
 *  record — nobody is their own parent. Matching reads exactly the fields
 *  `PaneFacts` already projects (orch group + role, tab id); no caller-side
 *  filtering of leads is required, and none should be added — the population
 *  the scan runs over is the whole fleet the caller hands in.
 *
 *  Each field it reads has a fixture that varies it (the lead-nesting corpus
 *  in `test/agentrows.test.ts`, the #1182 rule): the null-orch gate, the
 *  worker-role gate, the group pair, and the tab pair — the tab half held by
 *  fixtures whose operands COLLIDE (same group, different tab; tab null with
 *  the lead present), both red under group-only matching. The two-leads test
 *  cannot hold that half: its leads differ in group as well as tab. */
export function parentKey(facts: PaneFacts, fleet: readonly PaneFacts[]): string | null {
  const orch = facts.orch;
  if (orch === null || orch.role !== "worker") return null;
  const tab = facts.tab;
  if (tab === null) return null;
  const lead = fleet.find(
    (p) =>
      p.orch !== null &&
      p.orch.role === "lead" &&
      p.orch.group === orch.group &&
      p.tab !== null &&
      p.tab.id === tab.id,
  );
  return lead === undefined ? null : lead.key;
}
