// Which recorded sessions the sidebar's session list shows (#1592, #2116), as
// pure functions. Two independent controls, described under "WHY THERE ARE NOW
// TWO CONTROLS" below: the MODE picks whose sessions, and the delegate toggle
// then decides how much of an orchestration to show.
//
// WHAT THE NOISE ACTUALLY IS. An orchestration group mints a session per
// delegate and mints a NEW one on every rejoin, so a machine that has run a few
// fleets accumulates hundreds of worker/reviewer rows against a handful the
// human would ever click. None of them is a route the human needs: a delegate
// is respawned by its orchestrator, from the group's own roster, and the
// Orchestrations section resumes the group by its ORCHESTRATOR. So inside the
// orchestration view the default is the one session a human restarts by hand:
// the orchestrator's.
//
// THIS IS NOISE REDUCTION, NOT THE FIX FOR #1592. The startup hang was a
// BACKEND scaling defect (a sync per-group fan-out on the webview thread, an
// audit slurp, and an O(groups x store) probe), and it is fixed there. Hiding
// rows would not have fixed it and must never be offered as though it had:
// every one of those sessions is still scanned, still classified, and still
// available behind the toggle. What this changes is what a human has to read.
//
// BOTH RULES ARE STATED AS PROPERTIES, NOT AS ROLE LISTS. A row belongs to an
// orchestration when it has a recorded role AT ALL; within one, it is a
// delegate unless the role is `orchestrator`. Written the other way round -- a
// list of `worker | reviewer` to exclude -- either rule would go stale the
// moment a workflow file names a new role, and go stale SILENTLY, in the
// direction of showing hundreds of rows again. A new role is a delegate until
// somebody argues otherwise, which is the same default the orchestrator itself
// applies when it respawns one.

/** The orchestration identity of a session row, as `SessionBrowser.roleFor`
 *  resolves it: the durable roster record, the transcript-signature fallback,
 *  or `undefined` for a session with no orchestration identity at all. Only
 *  the role name is read here, so this takes the narrowest shape that says so
 *  rather than importing the whole `SessionRoleInfo`. */
export interface RoleOnly {
  role: string;
}

/** The one role an orchestration records that a human restarts by hand. */
export const OPERATOR_ROLE = "orchestrator";

// WHY THERE ARE NOW TWO CONTROLS, AND WHAT EACH ONE ANSWERS (#2116). They are
// different questions and conflating them into one three-state button was
// rejected on the design note:
//
//   MODE  — "whose sessions am I looking at?" A human's own panes, or an
//           orchestration's. Explicit, remembered per viewer, and the control
//           the human actually asked for.
//   THE DELEGATE TOGGLE (#1592) — "within an orchestration, do I want the
//           worker/reviewer rows too?" A reading preference about noise.
//
// They COMPOSE rather than replace: the mode picks the population, and the
// delegate toggle then acts INSIDE the orchestration population. In `mine`
// there are no delegates by construction — a row with no recorded role is not
// anybody's delegate — so the toggle is inert there and says nothing, which is
// what `hidden === 0` buys below.

/** Whose sessions the list is showing (#2116).
 *
 *  `mine` is the human's own panes: a session with no recorded orchestration
 *  identity at all. `orchestration` is everything an orchestration minted,
 *  orchestrator and delegates alike. The split is on the PROPERTY ("is there a
 *  recorded role?"), not on a role list, for the reason stated at the top of
 *  this file: a workflow naming a new role must not silently change which view
 *  its sessions appear in. */
export type SessionMode = "mine" | "orchestration";

/** The default view: the human's own sessions. */
export const DEFAULT_SESSION_MODE: SessionMode = "mine";

/** Read a persisted mode. TOTAL — anything that is not a mode this build knows
 *  yields the default rather than throwing or leaving the list in a state no
 *  control can name. `localStorage` is hand-editable and survives a downgrade,
 *  so a value written by a future build reaches this. */
export function decodeSessionMode(raw: unknown): SessionMode {
  return raw === "orchestration" ? "orchestration" : DEFAULT_SESSION_MODE;
}

/** Is this row part of an orchestration at all? The mode split, as one
 *  predicate, so the partition and any caller read the same answer. */
export function isOrchestrationSession(role: RoleOnly | undefined): boolean {
  return role !== undefined;
}

/** Is this row one the default list shows?
 *
 *  `undefined` -- no orchestration identity -- is the human's own session and
 *  is always shown. A recorded role is shown only when it is
 *  [`OPERATOR_ROLE`]; every other role, known or not yet invented, is a
 *  delegate. Comparison is on the trimmed value: the role reaches the frontend
 *  as a string off `agents.json`/the audit log, and a stray space must not
 *  promote a delegate or demote an orchestrator. */
export function isOperatorSession(role: RoleOnly | undefined): boolean {
  if (!role) return true;
  return role.role.trim() === OPERATOR_ROLE;
}

/** The rows to render, and how many the DELEGATE TOGGLE hid.
 *
 *  Two filters in one pass, in this order:
 *
 *   1. the MODE picks the population -- `mine` keeps rows with no recorded
 *      orchestration identity, `orchestration` keeps the rest;
 *   2. inside `orchestration`, and only there, the #1592 delegate rule then
 *      keeps operators unless `showDelegates` says otherwise.
 *
 *  `hidden` counts ONLY what step 2 held back, and that is the whole of how the
 *  two controls compose. A row the MODE excluded is not hidden, it is somewhere
 *  else -- one click away on a control that names where -- so counting it here
 *  would put a number on the delegate toggle for rows the delegate toggle
 *  cannot reveal. In `mine` there are no delegates at all, so `hidden` is 0 and
 *  the toggle has nothing to say (`delegateToggleLabel` returns `null`).
 *
 *  It is deliberately computed from the SAME pass rather than as
 *  `all.length - shown.length` by a caller: two subtractions of two filters is
 *  how a count and a list drift apart. When `showDelegates` is true nothing is
 *  hidden and `hidden` is 0 -- not "the number that would have been hidden",
 *  which would put a number on a toggle that is not currently hiding anything.
 *
 *  Never mutates its input, and preserves input order: the caller's array is
 *  the store's own list, already sorted newest-first by the scan. */
export function partitionSessions<T>(
  sessions: readonly T[],
  roleOf: (s: T) => RoleOnly | undefined,
  mode: SessionMode,
  showDelegates: boolean
): { shown: T[]; hidden: number } {
  const shown: T[] = [];
  let hidden = 0;
  for (const s of sessions) {
    const role = roleOf(s);
    if (isOrchestrationSession(role) !== (mode === "orchestration")) continue;
    if (mode === "orchestration" && !showDelegates && !isOperatorSession(role)) {
      hidden += 1;
      continue;
    }
    shown.push(s);
  }
  return { shown, hidden };
}

/** The toggle's label. Says what is hidden and what the click does, because a
 *  bare "Show all" next to a short list reads as a filter that is off. `null`
 *  when the toggle has nothing to say -- no delegate rows exist, so offering
 *  to reveal them would promise something that is not there. */
export function delegateToggleLabel(
  hidden: number,
  showDelegates: boolean,
  mode: SessionMode = DEFAULT_SESSION_MODE
): string | null {
  // Inert outside the orchestration view, whatever it is set to: there are no
  // delegate rows in `mine` for it to reveal, and offering to "hide agent
  // sessions" beside a list that has none reads as a filter that is on.
  if (mode !== "orchestration") return null;
  if (showDelegates) return "Hide agent sessions";
  if (hidden <= 0) return null;
  return hidden === 1
    ? "Show 1 hidden agent session"
    : `Show ${hidden} hidden agent sessions`;
}
