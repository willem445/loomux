// Pure geometry for hosting a plugin's child webview over a pane's content box
// (#360 Slice D). DOM-free — no live Tauri window involved, so this pins the
// "the embedded webview tracks the pane" contract without one. `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { pluginWebviewRect, pluginWindowShouldShow, frameUnchanged } from "../src/pluginwindow.ts";

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
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 640, height: 480 }), true);
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 0, height: 480 }), false);
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 640, height: 0 }), false);
  assert.equal(pluginWindowShouldShow({ left: 0, top: 0, width: 0, height: 0 }), false);
});

test("shouldShow ignores position — a pane scrolled off-screen is still shown, just repositioned", () => {
  assert.equal(pluginWindowShouldShow({ left: -5000, top: -5000, width: 100, height: 100 }), true);
});

// frameUnchanged (#414, rev-67 finding): the same-geometry skip decision for
// PluginPaneView.reposition() — a concurrent window-resize + sessions-panel
// transition can compute the identical (bounds, exclude) pair twice in one
// frame, and this is what tells the caller to skip the second atomic
// `plugin_set_frame` round trip.
const RECT = { x: 10, y: 20, width: 300, height: 200 };
const EXCLUDE_A = [{ x: 0, y: 0, width: 50, height: 50 }];
const EXCLUDE_B = [{ x: 0, y: 0, width: 60, height: 50 }];

test("frameUnchanged: null last (no call has ever succeeded) is never unchanged", () => {
  assert.equal(frameUnchanged(null, { rect: RECT, exclude: [] }), false);
});

test("frameUnchanged: identical bounds and identical exclude sets are unchanged", () => {
  const a = { rect: { ...RECT }, exclude: [{ ...EXCLUDE_A[0] }] };
  const b = { rect: { ...RECT }, exclude: [{ ...EXCLUDE_A[0] }] };
  assert.equal(frameUnchanged(a, b), true);
});

test("frameUnchanged: a bounds delta (any single field) is a change", () => {
  const last = { rect: RECT, exclude: [] };
  assert.equal(frameUnchanged(last, { rect: { ...RECT, x: RECT.x + 1 }, exclude: [] }), false);
  assert.equal(frameUnchanged(last, { rect: { ...RECT, width: RECT.width + 1 }, exclude: [] }), false);
});

test("frameUnchanged: an exclude delta is a change, even at the same bounds", () => {
  const last = { rect: RECT, exclude: EXCLUDE_A };
  assert.equal(frameUnchanged(last, { rect: RECT, exclude: EXCLUDE_B }), false); // exclude rect itself differs (width)
  assert.equal(frameUnchanged(last, { rect: RECT, exclude: [] }), false); // exclude cleared
  assert.equal(frameUnchanged({ rect: RECT, exclude: [] }, { rect: RECT, exclude: EXCLUDE_A }), false); // exclude appeared
});

test("frameUnchanged: a differing exclude COUNT is a change even if the shared entries match", () => {
  const last = { rect: RECT, exclude: EXCLUDE_A };
  const next = { rect: RECT, exclude: [EXCLUDE_A[0], EXCLUDE_A[0]] };
  assert.equal(frameUnchanged(last, next), false);
});
