// Pure geometry for hosting a plugin's WebviewWindow over a pane's content box
// (#360 Slice D). DOM-free — no live Tauri window involved, so this pins the
// "the overlay tracks the pane" contract without one. `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { pluginOverlayRect, pluginWindowShouldShow } from "../src/pluginwindow.ts";

test("the overlay rect is the main window's origin plus the pane's viewport offset", () => {
  const rect = pluginOverlayRect(
    { x: 100, y: 50 },
    { left: 220, top: 80, width: 640, height: 480 }
  );
  assert.deepEqual(rect, { x: 320, y: 130, width: 640, height: 480 });
});

test("a pane at the main window's own origin needs no translation", () => {
  const rect = pluginOverlayRect({ x: 0, y: 0 }, { left: 0, top: 0, width: 300, height: 200 });
  assert.deepEqual(rect, { x: 0, y: 0, width: 300, height: 200 });
});

test("fractional pixels round rather than accumulate drift", () => {
  const rect = pluginOverlayRect(
    { x: 10.4, y: 10.6 },
    { left: 5.5, top: 5.4, width: 100.6, height: 100.4 }
  );
  assert.deepEqual(rect, { x: 16, y: 16, width: 101, height: 100 });
});

test("width/height are floored at 1px — never zero or negative", () => {
  const rect = pluginOverlayRect({ x: 0, y: 0 }, { left: 0, top: 0, width: 0, height: -3 });
  assert.equal(rect.width, 1);
  assert.equal(rect.height, 1);
});

test("shouldShow is false the moment either dimension collapses to zero", () => {
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 640, height: 480 }), true);
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 0, height: 480 }), false);
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 640, height: 0 }), false);
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 0, height: 0 }), false);
});

test("shouldShow ignores position — a pane scrolled off-screen is still shown, just repositioned", () => {
  assert.equal(pluginWindowShouldShow({ left: -5000, top: -5000, width: 100, height: 100 }), true);
});
