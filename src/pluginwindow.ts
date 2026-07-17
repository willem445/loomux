// Pure geometry for hosting a plugin's isolated child `Webview` as a pane's
// content (#360 Slice D — see doc/design/pane-plugins.md's Isolation section
// and the multiwebview-embedding spike, fix/360-plugin-embed commit e337c95,
// findings comment on #360). Slice C's `plugin_open_window` embeds the
// webview into the MAIN window via `Window::add_child`; it does NOT position
// it — that's this slice's job, because only the frontend knows where the
// pane's content box currently sits.
//
// A plugin's child webview is a native region of the SAME top-level OS
// window as everything else in the app (not a separate window, unlike the
// overlay-window design this replaces), so "hosting it as the pane's
// content" means continuously repositioning and resizing that child webview
// to sit exactly over the pane's `.pane-content` box. `Webview.setPosition`/
// `setSize` (Tauri's `core:webview:*` commands) place a child webview
// RELATIVE TO ITS PARENT WINDOW'S OWN CLIENT AREA — standard Win32
// child-window semantics, confirmed against `Window::add_child`'s own
// contract — which is exactly the coordinate space `getBoundingClientRect()`
// already returns for any element in that window's document. Unlike the
// overlay-window design (a SEPARATE top-level `WebviewWindow`, positioned in
// absolute SCREEN coordinates, needing the main window's own scale factor
// and screen origin to translate into), there is no origin/DPI/multi-monitor
// translation step here at all: the pane's own client rect IS the webview
// rect, just rounded and floored at 1px. This module is that trivial pure
// arithmetic; pluginpaneview.ts is the DOM/Tauri wiring that calls it on
// every resize/move (DOM-free logic here so the "embedded webview tracks the
// pane" contract is unit-tested without a live Tauri window —
// test/pluginwindow.test.ts).

/** A DOM rect in logical/CSS pixels — the shape `getBoundingClientRect()`
 *  returns, reduced to the four fields this module needs. Already in the
 *  SAME coordinate space `Webview.setPosition`/`setSize` expect (both are
 *  relative to the parent window's own client area), so no translation is
 *  needed beyond rounding. */
export interface ElementRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** The position/size to hand `Webview.setPosition`/`setSize`, in logical
 *  pixels relative to the parent (main) window's own client area. */
export interface PluginWebviewRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Convert a pane's content-box rect (already relative to the main window's
 *  own client area, since that's what `getBoundingClientRect()` returns for
 *  any element in that window's document) into the rect to position the
 *  plugin's child webview at. Pure rounding/clamping: width/height are
 *  floored at 1px so a pane mid-collapse (a divider dragged to its minimum,
 *  a tab hidden mid-transition) never asks Tauri to size a webview to zero
 *  or negative pixels. */
export function pluginWebviewRect(rect: ElementRect): PluginWebviewRect {
  return {
    x: Math.round(rect.left),
    y: Math.round(rect.top),
    width: Math.max(1, Math.round(rect.width)),
    height: Math.max(1, Math.round(rect.height)),
  };
}

/** Whether the plugin webview should be shown at all right now. A content
 *  pane's box goes to zero width/height exactly when it isn't meant to be
 *  seen — a docked (minimized) pane's element is detached from the tree, a
 *  hidden project tab is `display:none`, and a maximized SIBLING pane hides
 *  every other pane the same way (grid.ts's own comment: "The container's
 *  `display:none` is what actually zeroes each pane's width"). Rather than
 *  wiring a bespoke hook into every one of those (dock, tab-switch,
 *  maximize-elsewhere all mutate the DOM differently), the plugin pane reuses
 *  the SAME zero-size signal `applyFit()` already uses to skip a PTY resize
 *  on a hidden pane — one predicate, one meaning, wherever it's read.
 *
 *  `overlayOpen` (#391, folded into #380): a plugin's child webview always
 *  paints ABOVE `main`'s own DOM content within its bounds and swallows
 *  pointer events there — a loomux overlay (the sessions browser, a modal, a
 *  context menu, …) opening over the pane's screen region does NOT zero its
 *  rect (the pane itself is still laid out normally underneath), so the
 *  rect-only check above can't see it. `overlayOpen` is the shared registry's
 *  `isOpen` (overlaystate.ts) threaded in by the caller — true forces a hide
 *  regardless of geometry, false defers to the rect exactly as before. */
export function pluginWindowShouldShow(rect: ElementRect, overlayOpen: boolean): boolean {
  return rect.width > 0 && rect.height > 0 && !overlayOpen;
}
