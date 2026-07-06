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
