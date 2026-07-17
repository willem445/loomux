// Pure geometry: which parts of a plugin pane are covered by open DOM
// overlays right now (#391, folded into #380 — the corrected root-cause fix
// superseding the reverted global-hide band-aid, PR #392). DOM-free — no live
// Tauri window or DOM element involved — so this pins the "the plugin's
// native child webview never bleeds over an open overlay" contract without
// one; pluginpaneview.ts is the DOM/Tauri wiring that calls it on every
// reposition (test/pluginocclusion.test.ts).
//
// The plugin's child webview is a native HWND `src-tauri/src/pluginregion.rs`
// clips with SetWindowRgn to punch DOM-overlay-shaped holes in it — see that
// module's doc comment for the full mechanism and why composition hosting was
// rejected. This module computes WHICH holes: for each currently-open overlay
// (overlaystate.ts), the part of its rect that overlaps the plugin's own pane
// rect, translated into the PANE's own top-left origin (0,0 = the pane's own
// corner) — the coordinate space `pluginregion::plugin_set_occlusion`'s
// `OcclusionRect` wire type expects, matching
// `pluginbroker::OpenPluginWindowRequest`'s own convention.

import type { ElementRect } from "./pluginwindow";

/** The rect to exclude from the plugin's own webview — pane-local logical
 *  pixels, matching `pluginregion::OcclusionRect` on the wire. */
export interface ExcludeRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** For each overlay rect, the part (if any) that overlaps `paneRect`,
 *  translated into the pane's own top-left origin. An overlay that doesn't
 *  overlap the pane at all contributes nothing — most overlays, most of the
 *  time, for any given plugin pane. Overlapping exclude rects are NOT merged
 *  here: `pluginregion.rs` combines them via successive region subtraction
 *  (`CombineRgn(..., RGN_DIFF)`), which is correct regardless of whether the
 *  input rects overlap each other, so there's no correctness reason to pay
 *  for a merge step here. */
export function computeExcludeRects(paneRect: ElementRect, overlayRects: ElementRect[]): ExcludeRect[] {
  const paneRight = paneRect.left + paneRect.width;
  const paneBottom = paneRect.top + paneRect.height;
  const out: ExcludeRect[] = [];
  for (const overlay of overlayRects) {
    const left = Math.max(paneRect.left, overlay.left);
    const top = Math.max(paneRect.top, overlay.top);
    const right = Math.min(paneRight, overlay.left + overlay.width);
    const bottom = Math.min(paneBottom, overlay.top + overlay.height);
    if (right <= left || bottom <= top) continue; // no overlap with this pane
    out.push({
      x: left - paneRect.left,
      y: top - paneRect.top,
      width: right - left,
      height: bottom - top,
    });
  }
  return out;
}
