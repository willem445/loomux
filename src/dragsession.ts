// Shared mousedown→mousemove→mouseup drag-session wiring for every divider
// drag in this codebase (grid.ts's pane splits, pane.ts's embed-slot and
// overlay dividers). Extracted after a #361 review: the embed/overlay
// dividers' own copy (mirroring grid.ts's own, pre-existing pattern) only
// cleaned up on `mouseup` — a drag that ends WITHOUT one (Alt-Tab away
// mid-drag fires window `blur`, not `mouseup`; the mouse button is still
// physically down, but the browser never delivers a matching up event to a
// window that no longer has focus) left the `mousedown`-applied state
// stranded. For grid.ts's `dragging` class that's a cosmetic leftover
// highlight; for the embed/overlay dividers' `.resizing` class
// (content-visibility: hidden on the panel's list, #361's drag-perf fix)
// the SAME gap leaves a docked view's list invisible until some LATER,
// unrelated resize happens to touch that side again — a real, not merely
// cosmetic, stuck state. One shared start/end-drag helper, used by every
// divider, means the fix (and any future one) only has to be made once.
//
// No pointercancel listener: every current caller wires plain MOUSE events
// (mousedown/mousemove/mouseup), never Pointer Events — pointercancel has
// no meaning for a listener set that never receives a pointerdown to
// cancel. If a caller ever migrates to Pointer Events, wire pointercancel
// there directly; this helper's `end` set (mouseup/blur/Escape) covers
// every path reachable through the events it actually listens for.

/** The subset of `Window` (or a fake, for `test/dragsession.test.ts`) this
 *  module touches — narrow, like domutil.ts's `Swappable`/`Reparentable`,
 *  so the exactly-once/no-stranded-listener invariant is unit-testable
 *  without a real DOM. `window` itself satisfies this structurally. */
export interface DragEventTarget {
  addEventListener(type: string, listener: (e: any) => void): void;
  removeEventListener(type: string, listener: (e: any) => void): void;
}

export interface DragSession {
  /** Called on every mousemove while the drag is live. */
  onMove: (e: MouseEvent) => void;
  /** Called EXACTLY ONCE when the drag ends, however it ends — the one
   *  place to tear down drag-only state (a CSS class, persisting the
   *  settled size, …). Ending early (blur/Escape) is treated exactly like
   *  a normal release: whatever size the drag reached stands, nothing
   *  reverts — this fixes the STRANDED-STATE bug, not a "cancel and
   *  restore the pre-drag size" feature nothing has asked for. */
  onEnd: () => void;
}

/** Wire a drag session's window-level listeners and guarantee `onEnd` fires
 *  exactly once — from mouseup, a window blur (the drag's real target no
 *  longer has focus to deliver a mouseup to), or Escape — never stranding
 *  whatever `mousedown`-time state the caller applied before calling this.
 *  `target` defaults to `window`; a caller never needs to pass it — it
 *  exists so the exactly-once/listeners-removed invariant can be pinned
 *  against a plain fake in `test/dragsession.test.ts`. */
export function startDragSession(session: DragSession, target: DragEventTarget = window): void {
  let ended = false;
  const end = () => {
    if (ended) return;
    ended = true;
    target.removeEventListener("mousemove", session.onMove as (e: any) => void);
    target.removeEventListener("mouseup", end);
    target.removeEventListener("blur", end);
    target.removeEventListener("keydown", onKeydown);
    session.onEnd();
  };
  const onKeydown = (e: KeyboardEvent) => {
    if (e.key === "Escape") end();
  };
  target.addEventListener("mousemove", session.onMove as (e: any) => void);
  target.addEventListener("mouseup", end);
  target.addEventListener("blur", end);
  target.addEventListener("keydown", onKeydown);
}
