// Pure geometry for hosting a plugin's isolated `WebviewWindow` as a pane's
// content (#360 Slice D — see doc/design/pane-plugins.md's Isolation section,
// "Decided: child WebviewWindow with scoped capabilities"). Slice C's
// `plugin_open_window` builds the window; it does NOT position it — that's
// this slice's job, because only the frontend knows where the pane's content
// box currently sits.
//
// A plugin's WebviewWindow is a separate top-level OS window, not a DOM node,
// so "hosting it as the pane's content" means continuously repositioning and
// resizing that OS window to sit exactly over the pane's `.pane-content` box.
// This module is the pure arithmetic for that; pluginpaneview.ts is the DOM/
// Tauri wiring that calls it on every resize/move (DOM-free logic here so the
// "overlay tracks the pane" contract is unit-tested without a live Tauri
// window — test/pluginwindow.test.ts).

export interface ScreenRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** The main window's client-area origin, in the SAME (logical/CSS-pixel) unit
 *  as a DOM element's `getBoundingClientRect()` — i.e. already converted from
 *  Tauri's physical-pixel `innerPosition()` via `toLogical(scaleFactor)`
 *  (pluginpaneview.ts does that conversion; this module only does arithmetic
 *  on already-logical numbers, so it never has to know about DPI itself). */
export interface WindowOrigin {
  x: number;
  y: number;
}

/** A DOM rect in logical/CSS pixels — the shape `getBoundingClientRect()`
 *  returns, reduced to the four fields this module needs. */
export interface ElementRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Convert a pane's content-box rect (viewport-relative, from
 *  `getBoundingClientRect()`) into an ABSOLUTE SCREEN rect for positioning
 *  the plugin's child window over it. Pure translation: the content box's
 *  offset from the main window's own viewport, added to where that viewport
 *  itself sits on screen. Width/height are floored at 1px so a pane mid-
 *  collapse (a divider dragged to its minimum, a tab hidden mid-transition)
 *  never asks Tauri to size a window to zero or negative pixels. */
export function pluginOverlayRect(origin: WindowOrigin, rect: ElementRect): ScreenRect {
  return {
    x: Math.round(origin.x + rect.left),
    y: Math.round(origin.y + rect.top),
    width: Math.max(1, Math.round(rect.width)),
    height: Math.max(1, Math.round(rect.height)),
  };
}

/** Whether the plugin window should be shown at all right now. A content
 *  pane's box goes to zero width/height exactly when it isn't meant to be
 *  seen — a docked (minimized) pane's element is detached from the tree, a
 *  hidden project tab is `display:none`, and a maximized SIBLING pane hides
 *  every other pane the same way (grid.ts's own comment: "The container's
 *  `display:none` is what actually zeroes each pane's width"). Rather than
 *  wiring a bespoke hook into every one of those (dock, tab-switch,
 *  maximize-elsewhere all mutate the DOM differently), the plugin pane reuses
 *  the SAME zero-size signal `applyFit()` already uses to skip a PTY resize
 *  on a hidden pane — one predicate, one meaning, wherever it's read. */
export function pluginWindowShouldShow(rect: ElementRect): boolean {
  return rect.width > 0 && rect.height > 0;
}
