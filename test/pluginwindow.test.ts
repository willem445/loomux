// Pure geometry for hosting a plugin's child webview over a pane's content box
// (#360 Slice D). DOM-free — no live Tauri window involved, so this pins the
// "the embedded webview tracks the pane" contract without one. `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { pluginWebviewRect, pluginWindowShouldShow } from "../src/pluginwindow.ts";

test("the webview rect is the pane's own client-area rect, rounded", () => {
  const rect = pluginWebviewRect({ left: 220, top: 80, width: 640, height: 480 });
  assert.deepEqual(rect, { x: 220, y: 80, width: 640, height: 480 });
});

test("a pane at the window's own origin needs no translation", () => {
  const rect = pluginWebviewRect({ left: 0, top: 0, width: 300, height: 200 });
  assert.deepEqual(rect, { x: 0, y: 0, width: 300, height: 200 });
});

test("fractional pixels round rather than accumulate drift", () => {
  const rect = pluginWebviewRect({ left: 5.5, top: 5.4, width: 100.6, height: 100.4 });
  assert.deepEqual(rect, { x: 6, y: 5, width: 101, height: 100 });
});

test("width/height are floored at 1px — never zero or negative", () => {
  const rect = pluginWebviewRect({ left: 0, top: 0, width: 0, height: -3 });
  assert.equal(rect.width, 1);
  assert.equal(rect.height, 1);
});

test("shouldShow is false the moment either dimension collapses to zero", () => {
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 640, height: 480 }, false), true);
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 0, height: 480 }, false), false);
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 640, height: 0 }, false), false);
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 0, height: 0 }, false), false);
});

test("shouldShow ignores position — a pane scrolled off-screen is still shown, just repositioned", () => {
  assert.equal(pluginWindowShouldShow({ left: -5000, top: -5000, width: 100, height: 100 }, false), true);
});

// #391 (folded into #380): an overlay opening over an otherwise-visible plugin
// pane doesn't touch its rect at all, so the hide has to come from a second
// signal threaded through explicitly.
test("shouldShow is false while an overlay is open, even with a perfectly valid rect", () => {
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 640, height: 480 }, true), false);
});

test("shouldShow returns to the rect's own answer once the overlay closes", () => {
  const rect = { left: 0, top: 0, width: 640, height: 480 };
  assert.equal(pluginWindowShouldShow(rect, true), false);
  assert.equal(pluginWindowShouldShow(rect, false), true);
});

test("overlayOpen doesn't mask an already-zero rect (nothing to hide twice, but never true)", () => {
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 0, height: 0 }, true), false);
});
