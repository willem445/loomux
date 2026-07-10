// Pure state machine for the non-blocking, streaming project search (issue #207).
//
// The backend (`ft_search_start`) walks off the UI thread and emits `ft-search`
// batches tagged with the search id the frontend issued. This module owns the
// two pieces of logic that must be correct for that to be safe, kept DOM-free so
// `node:test` can pin them:
//
//   1. Batching + cancellation. A search is a *session* identified by its id.
//      `accept` folds a batch into the session ONLY when the batch's id matches
//      the active session — a batch from a superseded search (new keystroke) or a
//      cancelled one (Esc) is dropped whole. This is the guarantee that "results
//      from a cancelled search never land": the FileEditView bumps the session id
//      before the old search can possibly finish, so its late/final events no-op.
//   2. Result capping. Even a legitimate search can hit tens of thousands of
//      matches; rendering them all would lock the DOM. `accept` stops
//      accumulating at `RENDER_CAP` and flags `overflow`, so the tree never holds
//      an unbounded result set (the backend also has its own ceiling, but the UI
//      must not depend on it).
//
// Plus the enumeration-source selection (`enumerationSource`) — which file set a
// search walks — since that decision (gitignore-aware by default) is testable
// intent, not DOM wiring.

import type { SearchMatch } from "./fileapi";

/** Max matches the UI accumulates for one search. Beyond this the tree can't be
 *  rendered responsively, so `accept` stops and flags `overflow` (surfaced as a
 *  "truncated" summary). Below the backend's own 5,000 ceiling on purpose — the
 *  UI cap is about DOM cost, which bites sooner than IPC payload size. */
export const RENDER_CAP = 2000;

/** One streamed batch from the backend, as delivered by the `ft-search` event.
 *  `done` is the terminal batch (empty `matches`, final `truncated`/`error`). */
export interface SearchBatch {
  id: number;
  matches: SearchMatch[];
  done: boolean;
  truncated: boolean;
  error?: string;
}

/** Accumulated state of one search session. */
export interface SearchState {
  /** Id of the search whose batches we accept; null when idle (no live search).
   *  Set it to a new id (or null) and every in-flight batch tagged with the old
   *  id is thereafter ignored — the cancellation primitive. */
  activeId: number | null;
  matches: SearchMatch[];
  /** The backend cut its walk short (its own match/file ceiling). */
  truncated: boolean;
  /** We stopped accumulating because `RENDER_CAP` was reached. */
  overflow: boolean;
  /** The active search delivered its terminal batch (finished, not cancelled). */
  done: boolean;
  /** The terminal batch carried an error code (e.g. the root vanished). */
  error?: string;
}

/** The idle state — no search running, nothing to show. */
export function idle(): SearchState {
  return { activeId: null, matches: [], truncated: false, overflow: false, done: true };
}

/** Begin a session for search `id`: a clean slate that will accept only `id`'s
 *  batches. The caller issues `id` (monotonic) and hands it to the backend. */
export function begin(id: number): SearchState {
  return { activeId: id, matches: [], truncated: false, overflow: false, done: false };
}

/** Fold one backend batch into `state`, returning the next state.
 *
 *  A batch whose id doesn't match `state.activeId` is dropped whole (the same
 *  `state` reference is returned) — this is the cancellation-race guard: once the
 *  view has moved to a newer search (or gone idle), the older search's remaining
 *  batches, including its terminal `done`, can never mutate the results.
 *
 *  Accumulation stops at `cap` matches; the batch that crosses the cap is sliced
 *  to fit and `overflow` latches true. */
export function accept(state: SearchState, batch: SearchBatch, cap: number = RENDER_CAP): SearchState {
  if (batch.id !== state.activeId) return state; // stale / cancelled → ignore

  let matches = state.matches;
  let overflow = state.overflow;
  if (!overflow && batch.matches.length > 0) {
    const room = cap - matches.length;
    if (batch.matches.length <= room) {
      matches = matches.concat(batch.matches);
    } else {
      matches = matches.concat(batch.matches.slice(0, Math.max(0, room)));
      overflow = true;
    }
  }

  return {
    activeId: state.activeId,
    matches,
    truncated: state.truncated || batch.truncated,
    overflow,
    done: state.done || batch.done,
    error: state.error ?? batch.error,
  };
}

/** Whether the results should be labelled truncated in the summary: either the
 *  backend cut its walk short or the UI hit its render cap. */
export function isTruncated(state: SearchState): boolean {
  return state.truncated || state.overflow;
}

/** Which file set a search enumerates.
 *  - `"git"`: `git ls-files` (tracked + untracked-unignored) — `.gitignore` is
 *    respected, so `node_modules`/build output are skipped for free.
 *  - `"walk"`: the full filesystem walk. */
export type EnumerationSource = "git" | "walk";

/** Pick the enumeration source. Default (`includeIgnored=false`) in a git repo
 *  respects `.gitignore` via `git ls-files`; the toggle, or a non-git root (which
 *  has no `.gitignore` to respect), walks the full tree. Mirrors the backend's
 *  `plan_enumeration` so the UI can describe what a search will do. */
export function enumerationSource(isGitRepo: boolean, includeIgnored: boolean): EnumerationSource {
  return isGitRepo && !includeIgnored ? "git" : "walk";
}
