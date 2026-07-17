// Shared "an overlay is open" registry (#391, folded into #380's review). A
// plugin pane's content is a NATIVE child webview (pluginpaneview.ts,
// Window::add_child) that always paints ABOVE `main`'s own DOM content within
// its bounds and swallows pointer events there — a known, accepted gap
// (doc/design/pane-plugins.md). Before this module, nothing told a plugin
// webview to get out of the way when a loomux DOM overlay opened over it: the
// webview only hid on a PANE-visibility change (tab switch, dock, maximize —
// pluginwindow.ts's `pluginWindowShouldShow`), and an overlay opening on TOP
// of an otherwise-visible pane doesn't touch that signal at all. The human hit
// this live — the sessions browser sidebar glitched for a few seconds before
// an unrelated layout recalc happened to hide the plugin underneath it.
//
// This is that missing signal: every DOM overlay in the app (the sessions
// browser, modals, context menus, in-pane overlays, …) calls `open()` when it
// opens and the returned closer when it closes; `isOpen`/`openCount` tell
// PluginPaneView (pluginpaneview.ts) to hide immediately, no matter which
// overlay it was or where the plugin pane sits in the layout — reused via
// `pluginwindow.ts`'s existing `pluginWindowShouldShow`, not a second hide
// mechanism.
//
// A class (not a bare module singleton) so tests can each build a fresh,
// isolated instance rather than sharing hidden module state — the same reason
// refreshgate.ts is a class. Production code imports the one shared
// `overlayState` instance below.

export type OverlayCloser = () => void;

export class OverlayRegistry {
  private count = 0;
  private listeners = new Set<(count: number) => void>();

  /** Register that one overlay instance just opened. Returns the matching
   *  closer — call it exactly once, on whichever path ends THIS overlay's
   *  lifetime (a close button, Escape, an outside click, the owning pane
   *  disposing while it's open — every one of them, not just the "normal"
   *  close). Idempotent: calling the returned closer more than once only
   *  decrements the count on its first call, so a caller that guards against
   *  double-close elsewhere doesn't have to duplicate that guard here. */
  open(): OverlayCloser {
    this.count++;
    this.notify();
    let closed = false;
    return () => {
      if (closed) return;
      closed = true;
      this.count = Math.max(0, this.count - 1);
      this.notify();
    };
  }

  /** Whether at least one overlay is currently open. */
  get isOpen(): boolean {
    return this.count > 0;
  }

  /** How many overlays are currently open (for assertions/debugging). */
  get openCount(): number {
    return this.count;
  }

  /** Be told every time the open count changes (not just the open/closed
   *  edge) — a PluginPaneView subscriber only needs the edge, but any future
   *  subscriber that wants a count (a status indicator, say) doesn't need a
   *  second mechanism. Returns an unsubscribe. */
  subscribe(fn: (count: number) => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  private notify(): void {
    for (const fn of this.listeners) fn(this.count);
  }
}

/** The one registry every loomux overlay call site shares. */
export const overlayState = new OverlayRegistry();
