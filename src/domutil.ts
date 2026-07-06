// Small DOM helpers shared across views.

/** The subset of a DOM node that {@link swapIfConnected} touches — narrow so
 *  the logic is unit-testable with a plain fake, no browser required. */
export interface Swappable {
  readonly isConnected: boolean;
  replaceWith(...nodes: unknown[]): void;
}

/**
 * Replace `from` with `to`, but only if `from` is still attached to the DOM.
 *
 * Inline editors (task-title rename, pane rename) bind their `commit()` to both
 * a key handler *and* `blur`. The key handler's swap detaches the focused input,
 * and detaching a focused element makes the browser fire `blur` — which invokes
 * `commit()` a second time. Calling `replaceWith` on the now-detached node throws
 * `NotFoundError: The node to be removed is no longer a child of this node.
 * Perhaps it was moved in a 'blur' event handler?`, which escapes the handler and
 * surfaces as the app-wide uncaught-error banner (see main.ts `showFatal`).
 *
 * Guarding on `isConnected` makes `commit()` idempotent: the first call swaps,
 * the redundant blur-driven call is a no-op. It also protects against a
 * background re-render having already removed the editor.
 *
 * Returns `true` if the swap happened, `false` if `from` was already detached.
 */
export function swapIfConnected(from: Swappable, to: unknown): boolean {
  if (!from.isConnected) return false;
  from.replaceWith(to);
  return true;
}

/** A node that {@link swapEditor} can inspect for both document-connectedness
 *  and mere parenthood — narrow enough to fake in unit tests. */
export interface Reparentable extends Swappable {
  readonly parentNode: unknown | null;
}

/** What {@link swapEditor} did, so the DOM caller knows whether it's safe to
 *  follow up with focus/relayout work. */
export interface EditorSwap {
  /** `to` took `from`'s place (`from` was still parented). */
  swapped: boolean;
  /** `from` was still on the live document — the ordinary Enter/Escape/blur
   *  path. Only then may the caller steal focus back to the terminal. */
  live: boolean;
}

/**
 * Swap an inline editor `from` back to its label `to`, tolerating a concurrent
 * detach *or* reparent of the editor's subtree — the pane-rename case in #113.
 *
 * `swapIfConnected` is enough for the task board, whose rows are thrown away and
 * rebuilt on every mutation: when a background re-render detaches the row, the
 * blur-driven commit no-ops and the orphaned input dies with the discarded row.
 * A pane is different — the grid *reuses* the same pane element, moving its whole
 * subtree with `replaceWith`/`replaceChildren` (grid.ts `swap` :631, `renderSplit`
 * :217, `collapse` :210) on spawn/kill/reorder. That move detaches the focused
 * rename input and fires a synchronous `blur` → the rename commit. A plain
 * `input.replaceWith(label)` here is either a silent no-op (input already
 * unparented) or swaps *within the detached subtree* — but the naive version then
 * leaves the reused header showing an orphaned `<input>` with the title vanished,
 * and its `focus()` call steals focus back mid-restructure.
 *
 * (Note: we could NOT reproduce the reported `NotFoundError` from static reading
 * of the grid — none of those `replaceWith` sites moves a node the blur handler
 * also touches. This helper is defensive-in-depth that provably makes the commit
 * safe *and* consistent under any concurrent detach/reparent; a stack capture in
 * main.ts's error listener is in place to pin the exact throwing frame if it
 * recurs live. See the #113 PR body.)
 *
 * So this helper distinguishes three states:
 *  - connected → swap and report `live` (caller refocuses): the normal path.
 *  - detached but still parented → swap *within the detached subtree* so the
 *    header is consistent (label back, no orphaned input) once the grid
 *    re-attaches it, but report `!live` so the caller does NOT steal focus.
 *  - detached and unparented → nothing to do (already gone): `swapped:false`.
 */
export function swapEditor(from: Reparentable, to: unknown): EditorSwap {
  if (from.isConnected) {
    from.replaceWith(to);
    return { swapped: true, live: true };
  }
  if (from.parentNode != null) {
    from.replaceWith(to);
    return { swapped: true, live: false };
  }
  return { swapped: false, live: false };
}
