// One reading out of the tab strip's published snapshot: is the ROSTER calling
// this pane's agent idle? (#2122 slice B, feeding slice A's
// `PaneActivity.noteRosterIdle`.)
//
// WHY THIS IS ITS OWN MODULE. The lookup is three nested absences deep — the
// group may not be in the payload, its summary may be a refusal, the agent may
// not be in the roster — and each of them has to answer `null` rather than
// `false`. Written inline at the call site in `main.ts` that is four
// hand-checked branches nobody can test; here it is a pure function over a
// literal.
//
// WHAT THE READING MEANS, AND WHAT IT DOES NOT. `idle_since_ms` is the idle
// REAPER's signal: "this agent holds no assignment / the reaper would consider
// killing it". It is emphatically NOT "parked at a prompt" (#2089), which is
// why `deriveAgentState` feeds it to the `idle` rung alone and never to
// `turn-done`. `doc/design/agents-tab.md` carries that distinction in full.
//
// NO IMPORT FROM `orchestration.ts`, deliberately. The shapes below are
// structural, so `StripViewPayload` satisfies them without this module reaching
// into a file whose transitive imports include the Tauri IPC seam — which is
// what keeps `node --test` able to load it, and what lets the tests build a
// three-field literal instead of a whole strip payload.

/** One agent row of a group's roster, as this module reads it. */
export interface RosterAgentReading {
  readonly id: string;
  /** Unix-ms the agent last went idle, or null while it holds work. */
  readonly idle_since_ms: number | null;
}

/** One group's slice of the strip. `summary` is null for a group the backend
 *  refused or has not published yet — a fact about the READ, not about the
 *  group. */
export interface RosterGroupReading {
  readonly summary: { readonly agents: readonly RosterAgentReading[] } | null;
}

/** The strip snapshot, narrowed to what this answer needs. */
export interface RosterReading {
  readonly groups: Record<string, RosterGroupReading | undefined>;
}

/** Whether the roster currently calls this pane's agent idle.
 *
 *  `null` means THE ROSTER DOES NOT COVER THIS PANE — no orchestration
 *  identity, a group the strip did not carry, a group whose summary was
 *  refused, or an agent id the roster does not hold. Every one of those is a
 *  failure to look, and `false` would be a positive claim ("this agent has
 *  work") derived from a lookup that found nothing. Today both resolve the pane
 *  to `working` through the ladder's `idle` rung, so the distinction costs
 *  nothing to honour and is the difference between an honest reading and a
 *  lucky one. */
export function rosterIdleFor(
  strip: RosterReading,
  group: string | null,
  agentId: string | null
): boolean | null {
  if (group === null || agentId === null) return null;
  const agents = strip.groups[group]?.summary?.agents;
  if (agents === undefined) return null;
  const agent = agents.find((a) => a.id === agentId);
  if (agent === undefined) return null;
  // `!== null`, never a truthiness test: `idle_since_ms` is a unix-ms
  // timestamp, and 0 is a legal one that `!!` would report as "has work".
  return agent.idle_since_ms !== null;
}
