// Which recorded sessions the sidebar's session list shows by default (#1592),
// as a pure function.
//
// WHAT THE NOISE ACTUALLY IS. An orchestration group mints a session per
// delegate and mints a NEW one on every rejoin, so a machine that has run a few
// fleets accumulates hundreds of worker/reviewer rows against a handful the
// human would ever click. None of them is a route the human needs: a delegate
// is respawned by its orchestrator, from the group's own roster, and the
// Orchestrations section resumes the group by its ORCHESTRATOR. So the default
// list is the sessions a human starts or restarts by hand.
//
// THIS IS NOISE REDUCTION, NOT THE FIX FOR #1592. The startup hang was a
// BACKEND scaling defect (a sync per-group fan-out on the webview thread, an
// audit slurp, and an O(groups x store) probe), and it is fixed there. Hiding
// rows would not have fixed it and must never be offered as though it had:
// every one of those sessions is still scanned, still classified, and still
// available behind the toggle. What this changes is what a human has to read.
//
// THE RULE IS STATED AS A PROPERTY, NOT AS A ROLE LIST. A row is shown when it
// is the human's own session (no recorded orchestration role at all) or the
// role is `orchestrator`. Everything else is a delegate. Written the other way
// round -- a list of `worker | reviewer` to exclude -- it would go stale the
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

/** The rows to render, and how many the default hid.
 *
 *  `hidden` is the count the toggle's label needs, and it is deliberately
 *  computed from the SAME pass rather than as `all.length - shown.length` by a
 *  caller: two subtractions of two filters is how a count and a list drift
 *  apart. When `showDelegates` is true nothing is hidden and `hidden` is 0 --
 *  not "the number that would have been hidden", which would put a number on a
 *  toggle that is not currently hiding anything.
 *
 *  Never mutates its input, and preserves input order: the caller's array is
 *  the store's own list, already sorted newest-first by the scan. */
export function partitionSessions<T>(
  sessions: readonly T[],
  roleOf: (s: T) => RoleOnly | undefined,
  showDelegates: boolean
): { shown: T[]; hidden: number } {
  if (showDelegates) return { shown: sessions.slice(), hidden: 0 };
  const shown: T[] = [];
  let hidden = 0;
  for (const s of sessions) {
    if (isOperatorSession(roleOf(s))) shown.push(s);
    else hidden += 1;
  }
  return { shown, hidden };
}

/** The toggle's label. Says what is hidden and what the click does, because a
 *  bare "Show all" next to a short list reads as a filter that is off. `null`
 *  when the toggle has nothing to say -- no delegate rows exist, so offering
 *  to reveal them would promise something that is not there. */
export function delegateToggleLabel(hidden: number, showDelegates: boolean): string | null {
  if (showDelegates) return "Hide agent sessions";
  if (hidden <= 0) return null;
  return hidden === 1
    ? "Show 1 hidden agent session"
    : `Show ${hidden} hidden agent sessions`;
}
