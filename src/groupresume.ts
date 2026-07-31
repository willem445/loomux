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
//
// ONE PLAN IS ONE GROUP (#485). A tab can hold panes from two orchestration
// groups (split an orchestrator tab, launch a second orchestrator into it —
// #481/#478 makes that the primary gesture). This module used to be handed
// "every dormant orch placeholder in the tab" and answer with a single plan:
// one orchestrator kept, the other silently dropped, and the dropped group's
// delegates rejoined into the survivor. So the member set is now partitioned
// by each member's OWN recorded group (`partitionByGroup`) before planning,
// the plan refuses any member naming a different group (`foreign`), and a set
// that can't be attributed to one group at all is refused outright
// (`ambiguous`) instead of resolved by preference. The backend enforces the
// same rule at the join point (`resume_recorded_session`), so this is the
// legible half of the guarantee, not the whole of it.

/** One recorded group member to (maybe) bring back. */
export interface GroupMember {
  sessionId: string;
  /** "orchestrator" | "worker" | "reviewer" | "planner". */
  role: string;
  /** The group this member's OWN captured record names (#485). A tab can hold
   *  panes from two groups, so "which group is this member's" is a property of
   *  the MEMBER, never of the tab it happens to sit in. Null/absent for a
   *  placeholder captured before #485 (no recorded group) — see `planGroupResume`
   *  for how those are treated. */
  groupId?: string | null;
}

/** Normalize a captured group id: absent, null, or blank all mean "this record
 *  doesn't name a group", never a group literally named "". */
function normGroup(v: string | null | undefined): string | null {
  const s = (v ?? "").trim();
  return s ? s : null;
}

/** Split captured orch placeholders into the ones belonging to `group` and the
 *  rest (#485). Used twice by the wiring, and it must be the SAME rule both
 *  times: to pick the members one Resume click plans, and to pick which dormant
 *  placeholders that click may clear afterwards — a placeholder belonging to
 *  another group must survive a resume it was never part of.
 *
 *  Belonging is by recorded group id, with blank normalized to null, so a
 *  pre-#485 snapshot (every record null) still forms ONE set exactly as before —
 *  and a click on a null-group placeholder never claims a record that DOES name
 *  a group. */
export function partitionByGroup<T extends { groupId?: string | null }>(
  records: readonly T[],
  group: string | null
): { mine: T[]; others: T[] } {
  const want = normGroup(group);
  const mine: T[] = [];
  const others: T[] = [];
  for (const r of records) (normGroup(r.groupId) === want ? mine : others).push(r);
  return { mine, others };
}

/** The ordered whole-group resume plan. `orchestrator` runs first (relaunches the
 *  group), then every `rejoin` member (backend re-registers it), and `skipped`
 *  members are reported but not resumed (no transcript → would be a dead pane).
 *  `orchestrator` + `rejoin` + `skipped` together cover every member with a
 *  session id — one click, one plan for the whole set. */
export interface GroupResumePlan {
  orchestrator: GroupMember | null;
  /** True when a captured orchestrator EXISTS but its own session has no
   *  transcript to resume. The whole group can't be relaunched cleanly (a
   *  `--resume` of a deleted orchestrator conversation lands in a dead pane), so
   *  the caller falls back to the session browser instead — the same honest
   *  degradation delegates get, applied to the orchestrator (#194.5). Distinct
   *  from "no orchestrator captured at all" (both → browser, but different copy). */
  orchestratorUnresumable: boolean;
  rejoin: GroupMember[];
  skipped: GroupMember[];
  /** Members whose own record names a DIFFERENT group than the one being
   *  resumed (#485). NEVER rejoined — rejoining one is the cross-group
   *  contamination this plan exists to make unrepresentable, so they are
   *  reported to the caller instead of quietly folded into `rejoin`. */
  foreign: GroupMember[];
  /** True when this member set cannot be attributed to ONE group: more than one
   *  distinct orchestrator session survives the group filter, and they don't all
   *  name a group (a pre-#485 snapshot records no group at all, so two
   *  orchestrators in one tab are indistinguishable from two groups).
   *
   *  The whole set is then unplannable — `orchestrator` is null — because the
   *  alternative is what #485 filed: keep one orchestrator, drop the other
   *  without a word, and rejoin delegates into whichever survived. The caller
   *  fails the click loudly and points at the session browser, where each
   *  session's own group is known. */
  ambiguous: boolean;
}

/** Plan a whole-group resume from its captured members and a resumability
 *  predicate (does this session id still have a transcript on disk — built from
 *  `listSessions()` in the wiring, so this stays pure).
 *
 *  `group` is the group being resumed — the clicked placeholder's OWN recorded
 *  group, not the tab's binding (#485). When given, any member whose record
 *  names a different group is refused into `foreign` rather than rejoined.
 *  Omitted/null keeps the pre-#485 behavior for records that name no group. */
export function planGroupResume(
  members: readonly GroupMember[],
  resumable: (sessionId: string) => boolean,
  group?: string | null
): GroupResumePlan {
  const target = normGroup(group);
  let orchRecord: GroupMember | null = null;
  // Every orchestrator record that survived the group filter, so "we cannot
  // tell which group this set belongs to" is detectable rather than resolved
  // by a silent preference (#485).
  const orchCandidates: GroupMember[] = [];
  const rejoin: GroupMember[] = [];
  const skipped: GroupMember[] = [];
  const foreign: GroupMember[] = [];
  // Members are expected to be unique per session id, but dedup here anyway so a
  // duplicated record can't plan the same agent twice (the latch guards at the
  // group level, not per member).
  const seen = new Set<string>();
  for (const m of members) {
    if (!m.sessionId) continue; // nothing to resume without an id
    if (seen.has(m.sessionId)) continue;
    seen.add(m.sessionId);
    // #485, the structural half of the frontend: a member that names a group
    // is planned for THAT group only. It can never reach `rejoin` here, so no
    // amount of caller confusion downstream can turn it into a rejoin into
    // the group being resumed. A member naming no group (pre-#485 capture)
    // isn't provably foreign, so it stays in — the `ambiguous` check below is
    // what keeps that from silently mixing two groups.
    const own = normGroup(m.groupId);
    if (target && own && own !== target) {
      foreign.push(m);
      continue;
    }
    if (m.role === "orchestrator") {
      orchCandidates.push(m);
      // One orchestrator per group; if more than one is captured, prefer a
      // resumable record so the relaunch has a conversation to resume.
      if (!orchRecord || (!resumable(orchRecord.sessionId) && resumable(m.sessionId))) {
        orchRecord = m;
      }
      continue;
    }
    (resumable(m.sessionId) ? rejoin : skipped).push(m);
  }
  // Two orchestrators that don't BOTH name a group can't be told apart from
  // two groups sharing a tab, and picking one of them is precisely the #485
  // defect. Refuse the whole set instead. (Both naming a group — only possible
  // once #485's per-pane capture exists, and both then equal to `target` by the
  // filter above — is a genuine duplicate record, and keeps the resumable-wins
  // tie-break above.)
  const ambiguous =
    orchCandidates.length > 1 && !orchCandidates.every((m) => normGroup(m.groupId) !== null);
  let orchestrator: GroupMember | null = null;
  let orchestratorUnresumable = false;
  // Gate the orchestrator on the transcript predicate too — a stale orchestrator
  // session shouldn't relaunch into a dead pane.
  if (orchRecord && !ambiguous) {
    if (resumable(orchRecord.sessionId)) orchestrator = orchRecord;
    else orchestratorUnresumable = true;
  }
  return { orchestrator, orchestratorUnresumable, rejoin, skipped, foreign, ambiguous };
}
