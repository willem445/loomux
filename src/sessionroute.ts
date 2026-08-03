// Pure Sessions-tab restore routing (#781). DOM/IPC-free, like
// restoredecision.ts and groupresume.ts: main.ts's `restoreSession` asks this
// module what a clicked row means and then executes that route — opening the
// group rejoin, or a plain pane — so the RULE is unit-tested
// (test/sessionroute.test.ts) without a DOM, a pane, or the backend.
//
// THE RULE IS RECORDED MEMBERSHIP, NEVER THE CLI. A session rejoins its group
// iff loomux has a record that it belonged to one — a roster row from
// `orch_session_roles`, or the transcript-signature fallback the session
// browser derives (`SessionsPanel.roleFor`). Which agent CLI wrote the session
// is not part of that question, and must never become part of it again.
//
// It was, until this module existed: the route gated on `source === "claude"`,
// written when copilot session ids were not tracked at all, and nothing
// re-derived it once `spawn_copilot_session_watcher` started recording them for
// group agents AND for the orchestrator. The result was a copilot orchestrator
// session that showed its `ORCH` chip and the chip's own "click to restore the
// whole orchestration" tooltip, then restored as a bare `copilot --resume=<id>`
// — no `--additional-mcp-config`, no `--add-dir`, no allow-all posture, no
// model, no persona, no group binding. A pane that looks like its agent and can
// do none of its work, which is precisely what `docs/orchestration.md` promises
// this path never produces.
//
// Copilot 1.0.76 made that failure quieter still: per the CLI's changelog
// (1.0.76, 2026-07-29 — "Resuming a session now restores its autopilot or plan
// mode instead of reverting to interactive"), a bare resume comes back IN
// autopilot mode, so the half-configured pane reads as a healthy autopilot
// agent while carrying none of the group's wiring.
//
// A session with no recorded membership still restores plain, and that is not
// the same failure: loomux has no evidence it was ever in a group, the row
// carries no chip claiming otherwise, and a plain pane is exactly what it is.

/** The recorded orchestration membership of a session — the fields the route
 *  needs out of the session browser's `SessionRoleInfo`. */
export interface RecordedRole {
  group_id: string;
  role: string;
}

/** Just enough of a `listSessions()` row to route it. */
export interface RoutableSession {
  /** The CLI that wrote the session ("claude" | "copilot"). Deliberately NOT
   *  part of the route decision — it only labels the plain pane. */
  source: string;
  title: string;
}

/** Where a clicked Sessions-tab row restores to. */
export type SessionRestoreRoute =
  | { kind: "orchestration"; groupId: string; role: string }
  | { kind: "plain"; paneName: string };

/** Longest session title kept intact in a plain pane's name; anything longer is
 *  truncated with an ellipsis so a pane tab stays readable. */
const PANE_NAME_TITLE_LIMIT = 34;

/** Route one Sessions-tab click.
 *
 *  @param s    the clicked row (its `source` labels a plain pane; it never
 *              decides the route — see the module comment).
 *  @param role the session's recorded orchestration membership, or `undefined`
 *              when loomux has none.
 */
export function sessionRestoreRoute(
  s: RoutableSession,
  role: RecordedRole | undefined
): SessionRestoreRoute {
  if (role) {
    return { kind: "orchestration", groupId: role.group_id, role: role.role };
  }
  const title =
    s.title.length > PANE_NAME_TITLE_LIMIT
      ? s.title.slice(0, PANE_NAME_TITLE_LIMIT) + "…"
      : s.title;
  return { kind: "plain", paneName: `${s.source === "claude" ? "claude" : "copilot"} · ${title}` };
}
