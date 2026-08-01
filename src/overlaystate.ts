// Shared "which DOM overlays are open, and where" registry (#391, folded into
// #380 — the corrected root-cause fix superseding the reverted global-hide
// band-aid, PR #392, reverted at d3333b3). A plugin pane's content is a
// NATIVE child webview (pluginpaneview.ts, Window::add_child) that always
// paints ABOVE `main`'s own DOM content within its bounds and swallows
// pointer events there — see `src-tauri/src/pluginregion.rs`'s module doc
// comment for the full root-cause writeup and the composition-hosting spike
// that ruled out a DOM-side-only fix. `pluginregion::plugin_set_occlusion`
// fixes this at the OS level by clipping the plugin's own HWND to exclude
// whatever overlay rects currently cover its pane — this registry is the
// "which rects, right now" half of that: every DOM overlay in the app calls
// `open()` with a live rect-getter when it opens and the returned closer when
// it closes; `currentRects()`/`subscribe()` let PluginPaneView
// (pluginpaneview.ts) recompute and re-clip immediately on every open/close
// edge, no lag.
//
// A class (not a bare module singleton) so tests can each build a fresh,
// isolated instance rather than sharing hidden module state — the same reason
// refreshgate.ts is a class. Production code imports the one shared
// `overlayState` instance below.
//
// Pure/DOM-free by design: `open()` takes an opaque rect-GETTER, never an
// `HTMLElement` or a `ResizeObserver` — the registry itself never touches the
// DOM, so it's unit-tested the same way the pre-#391 boolean version was
// (test/overlaystate.test.ts). The DOM wiring (an overlay's own element,
// `getBoundingClientRect()`, a `ResizeObserver` to `poke()` on a
// resize-while-open) lives at each call site, hand-validated per CLAUDE.md's
// convention for DOM wiring.
//
// EVERY covering DOM overlay in this codebase is either wired into this
// registry or deliberately excluded below — the point of listing exclusions
// explicitly is that a reviewer (or a future contributor adding a new
// overlay) can check "is this one already covered?" without re-deriving the
// reasoning from scratch.
//
// Wired: modal.ts's modal()/promptModal(), editor.ts's editorConfigDialog,
// contextmenu.ts's showContextMenu, gitview.ts's own hand-rolled menu,
// tabbar.ts's menu/preview/palette popovers, pane.ts's six in-pane overlay
// toggles, and toast.ts's showToast — wired via `open()`/the returned closer,
// exactly the "this rect currently covers a plugin pane" contract this
// registry is for. Unlike the reverted global-hide PR (a single global
// boolean, so wiring a toast would have hidden EVERY plugin pane in the app
// for the toast's ~5s lifetime even where it doesn't visually overlap one at
// all), this registry is per-rect (the whole point of the #391 redo — see
// pluginregion.rs), so a toast only ever punches a hole the size of the toast
// itself.
//
// Wired differently: sessions.ts's SessionBrowser calls `poke()` alone, NEVER
// `open()`/the closer (#380 round 2). The sessions sidebar was originally
// wired the same way every other entry on this list is (the bug #391 was
// reported through), but `#sessions` (`styles.css`) turned out to be a flex
// SIBLING of the pane grid, not a `position: absolute`/`fixed` covering
// layer — it never overlaps a plugin pane's rect at any point in its own
// `width` transition, so it never had a meaningful rect to register.
// `poke()` still does exactly what its own doc comment says — "force
// subscribers to recompute without an open/close edge" — it just doesn't
// require a prior `open()` to make sense: any call site whose OWN geometry
// change should prompt every plugin pane to recompute ITS OWN bounds (not
// this call site's coverage) can call `poke()` on its own, registering
// nothing. `sessions.ts`'s header comment has the full writeup.
//
// Deliberately excluded (carried over from the #391 rev-97 list — none of
// these were excluded because of the "global hide is too broad" cost the
// per-rect upgrade fixes, so the reasoning is unchanged):
//   - restoresplash.ts's boot splash — shown before any pane (let alone a
//     plugin pane) exists; structurally cannot overlap one.
//   - launcher.ts's welcome form — it IS a pane's own persistent content
//     ("closed by closing the pane itself"), not a dismissable overlay that
//     opens over OTHER content the way everything else on this list is.
//   - tasksview.ts's nested approve/request-changes dialogs — `position:
//     absolute` bounded by the already-registered tasks overlay's own box
//     (a Pane's `toggleTasksView`, above); opening one doesn't add coverage
//     the tasks overlay itself hasn't already registered.
//   - grid.ts's drag-drop ghost/indicator — transient, drag-gesture-only,
//     follows the cursor rather than covering a fixed region for any
//     meaningful duration.

import type { ElementRect } from "./pluginwindow";

export type OverlayCloser = () => void;

/** Why `subscribe()`'s callback fired — passed through so a subscriber that
 *  cares (PluginPaneView, for its breadcrumb's trigger-source label) doesn't
 *  have to guess; one a subscriber doesn't care about (the pre-#380 shape)
 *  can simply ignore the argument. */
export type OverlayChangeReason = "open" | "close" | "poke";

export class OverlayRegistry {
  private overlays = new Map<number, () => ElementRect | null>();
  private nextId = 0;
  private listeners = new Set<(reason: OverlayChangeReason) => void>();

  /** Register that one overlay instance just opened, tracked by a live rect
   *  getter — called fresh every time `currentRects()` runs, never cached, so
   *  an overlay that moves/resizes while open is always read correctly
   *  without a separate "moved" event of its own. Returns the matching
   *  closer — call it exactly once, on whichever path ends THIS overlay's
   *  lifetime (a close button, Escape, an outside click, the owning pane
   *  disposing while it's open — every one of them, not just the "normal"
   *  close). Idempotent: calling the returned closer more than once only
   *  removes it on its first call. */
  open(getRect: () => ElementRect | null): OverlayCloser {
    const id = this.nextId++;
    this.overlays.set(id, getRect);
    this.notify("open");
    let closed = false;
    return () => {
      if (closed) return;
      closed = true;
      this.overlays.delete(id);
      this.notify("close");
    };
  }

  /** Whether at least one overlay is currently open. */
  get isOpen(): boolean {
    return this.overlays.size > 0;
  }

  /** How many overlays are currently open (for assertions/debugging). */
  get openCount(): number {
    return this.overlays.size;
  }

  /** Every currently-open overlay's rect, read live right now — never a
   *  stale snapshot. An overlay whose getter returns null (e.g. its element
   *  was detached from the document without going through its own close
   *  path) contributes nothing rather than throwing. */
  currentRects(): ElementRect[] {
    const out: ElementRect[] = [];
    for (const getRect of this.overlays.values()) {
      const r = getRect();
      if (r) out.push(r);
    }
    return out;
  }

  /** Be told every time the open set changes (not just the open/closed edge,
   *  and not just from `open()`/the closer — `poke()` below fires it too) —
   *  a PluginPaneView subscriber only needs "something might have changed,
   *  recompute", so any edge is enough. Returns an unsubscribe. */
  subscribe(fn: (reason: OverlayChangeReason) => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  /** Force subscribers to recompute without an open/close edge — a call site
   *  whose OWN registered overlay can resize/move WHILE open (a
   *  `ResizeObserver` on its own element, say) calls this instead of
   *  re-opening a new slot. Does NOT require a matching `open()`, though (#380
   *  round 2): a call site whose geometry change affects every plugin pane's
   *  own BOUNDS rather than what covers them — never overlapping a pane, so
   *  never a rect worth registering — can call `poke()` on its own, with no
   *  `open()` at all. `sessions.ts`'s `SessionBrowser` is that case; see its
   *  header comment. */
  poke(): void {
    this.notify("poke");
  }

  private notify(reason: OverlayChangeReason): void {
    for (const fn of this.listeners) fn(reason);
  }
}

/** The one registry every loomux overlay call site shares. */
export const overlayState = new OverlayRegistry();
