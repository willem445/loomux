// When the issues view must re-resolve its writable label vocabulary, and what
// the veto spelling becomes when a resolve fails (#2663, rev-std round 1).
//
// DOM-free and pure so it can be tested (`test/labelvocab.test.ts`) — the view
// that uses it is hand-validated glue, which is exactly why the decision lives
// here instead of inside it.
//
// THE DEFECT THIS REPLACES. `holdLabel` was resolved once per REPO change, on
// the argument that "a pane's group is fixed for the life of the pane". Two
// things falsify that:
//
//  1. A pane can GAIN a group mid-life — `applyOrchIdentity` runs from
//     `respawnFresh` too, which is #407's in-place promotion — and the issues
//     view survives it (it is disposed only with the pane).
//  2. Even with a fixed group, `apply_workflow` (#1689 B) rewrites that group's
//     `guardrails.intake` under a view that is already open.
//
// Either way the button offers a spelling the now group-scoped allow-list
// refuses, and the veto errors until the human changes repo or reopens the
// view. It fails CLOSED — a stale value can only be a previous hold spelling,
// and the backend allow-list still gates every write, so no wrong label can
// land — but "the veto stops working until you reopen" is the #778 failure
// wearing a different hat, so the window is closed rather than documented.

/** What a resolved vocabulary is resolved FOR. */
export interface VocabScope {
  /** Repository root, or null before the first resolve. */
  repo: string | null;
  /** The pane's orchestration group id, or null for a plain pane. */
  group: string | null;
}

/** Did the scope change — i.e. is a cached spelling now about something else? */
export function scopeChanged(prev: VocabScope, next: VocabScope): boolean {
  return prev.repo !== next.repo || prev.group !== next.group;
}

/**
 * Should the vocabulary be re-resolved on this refresh?
 *
 * **`true` whenever a group is in play**, and that is the point rather than an
 * oversight: a group's vocabulary is mutable under an open view (`apply_workflow`),
 * and no event reaches the frontend when it moves, so scope equality cannot
 * decide it — the scope is identical across an apply. Re-asking is a registry
 * map read behind one IPC call, on a refresh that already spawns two `gh`
 * processes over the network; the cache was never buying anything measurable.
 *
 * With no group, the pre-#2663 behaviour is kept exactly: resolve on a scope
 * change only. That arm reads and parses a file, and a plain pane has no
 * mechanism that rewrites its answer mid-session.
 */
export function shouldReresolve(prev: VocabScope, next: VocabScope): boolean {
  return scopeChanged(prev, next) || next.group !== null;
}

/**
 * The spelling to hold after a resolve attempt.
 *
 * `fetched` is null when the call REJECTED, and empty when it resolved to
 * nothing. The two are treated alike as "no answer", but what follows depends
 * on whether a previous answer is still about the same scope:
 *
 * - a scope change discards `previous` (it describes a different repo or group),
 *   so no answer means the built-in — the value the backend's own allow-list
 *   falls back to for a repo whose file it also could not read;
 * - within one scope, a transient failure keeps the last good answer rather
 *   than silently retracting a rename. Resetting to the built-in there would
 *   make a blipped IPC call look exactly like "this group stopped renaming the
 *   veto", and the button would start writing a label the poller ignores.
 */
export function resolvedHold(
  fetched: string | null | undefined,
  previous: string | null,
  builtin: string
): string {
  const got = (fetched ?? "").trim();
  if (got) return got;
  return previous ?? builtin;
}
