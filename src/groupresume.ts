// Pure planning for a WHOLE-group resume (#194 P4, demo round 3). When the human
// clicks Resume on a dormant restored orchestration group, that one click is
// consent to bring the entire group back — not just the orchestrator. This module
// turns the group's recorded roster (from the backend `orchSessionRoles`) into an
// ordered plan; the wiring (main.ts) executes it through the existing
// `resumeOrchSession` machinery. DOM/IPC-free so the planning is unit-tested.
//
// THE ORDER MATTERS. The orchestrator must come back FIRST: resuming its session
// relaunches the group's control plane (MCP identity, task board) and makes the
// group live. Only then can a worker/reviewer/planner rejoin — the backend
// refuses a delegate rejoin into a group that isn't live. So the plan separates
// the orchestrator from the delegates, and the wiring awaits the orchestrator
// before the delegates.
//
// FALLBACK PER MEMBER. A delegate is rejoined by RESUMING its recorded session
// (`--resume` into the idle TUI, credit-neutral, no prompt replay — the same rule
// as agent panes) via the backend, which re-registers it with the group so the
// orchestrator can still message it. But a delegate whose session was never
// prompted has no transcript on disk, so `--resume` would fail ("No conversation
// found …") and strand a dead pane. We can't spawn a FRESH group-registered
// worker from the frontend (only the orchestrator spawns delegates), so such a
// member is put in `skipped` — reported, not resumed into a dead pane; the
// orchestrator can respawn a fresh worker on demand once it's live.

/** One recorded group member to (maybe) bring back. */
export interface GroupMember {
  sessionId: string;
  /** "orchestrator" | "worker" | "reviewer" | "planner". */
  role: string;
}

/** The ordered whole-group resume plan. `orchestrator` runs first (relaunches the
 *  group), then every `rejoin` member (backend re-registers it), and `skipped`
 *  members are reported but not resumed (no transcript → would be a dead pane).
 *  `orchestrator` + `rejoin` + `skipped` together cover every member with a
 *  session id — one click, one plan for the whole set. */
export interface GroupResumePlan {
  orchestrator: GroupMember | null;
  rejoin: GroupMember[];
  skipped: GroupMember[];
}

/** Plan a whole-group resume from its roster and a resumability predicate (does
 *  this session id still have a transcript on disk — built from `listSessions()`
 *  in the wiring, so this stays pure). */
export function planGroupResume(
  members: readonly GroupMember[],
  resumable: (sessionId: string) => boolean
): GroupResumePlan {
  let orchestrator: GroupMember | null = null;
  const rejoin: GroupMember[] = [];
  const skipped: GroupMember[] = [];
  // `session_roles()` emits one row per roster record and is expected to be unique
  // per session id, but dedup here anyway so a duplicated row can't plan the same
  // agent twice (which the latch guards at the group level, not per member).
  const seen = new Set<string>();
  for (const m of members) {
    if (!m.sessionId) continue; // nothing to resume without an id
    if (seen.has(m.sessionId)) continue;
    seen.add(m.sessionId);
    if (m.role === "orchestrator") {
      // One orchestrator per group; if the roster somehow lists more than one,
      // prefer a resumable record so the relaunch has a conversation to resume.
      if (!orchestrator || (!resumable(orchestrator.sessionId) && resumable(m.sessionId))) {
        orchestrator = m;
      }
      continue;
    }
    (resumable(m.sessionId) ? rejoin : skipped).push(m);
  }
  return { orchestrator, rejoin, skipped };
}
