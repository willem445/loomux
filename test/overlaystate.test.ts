// The shared "which DOM overlays are open, and where" registry (#391, folded
// into #380). DOM-free — no overlay actually opens here, just the
// tracking/notify contract every overlay call site and PluginPaneView rely
// on. `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { OverlayRegistry } from "../src/overlaystate.ts";
import { computeExcludeRects } from "../src/pluginocclusion.ts";

const RECT_A = { left: 0, top: 0, width: 10, height: 10 };
const RECT_B = { left: 20, top: 20, width: 5, height: 5 };

test("starts closed", () => {
  const reg = new OverlayRegistry();
  assert.equal(reg.isOpen, false);
  assert.equal(reg.openCount, 0);
  assert.deepEqual(reg.currentRects(), []);
});

test("open() marks it open and its rect is live", () => {
  const reg = new OverlayRegistry();
  reg.open(() => RECT_A);
  assert.equal(reg.isOpen, true);
  assert.equal(reg.openCount, 1);
  assert.deepEqual(reg.currentRects(), [RECT_A]);
});

test("the returned closer closes it and its rect stops being reported", () => {
  const reg = new OverlayRegistry();
  const close = reg.open(() => RECT_A);
  close();
  assert.equal(reg.isOpen, false);
  assert.equal(reg.openCount, 0);
  assert.deepEqual(reg.currentRects(), []);
});

test("two overlays open at once — only the LAST close reports closed", () => {
  const reg = new OverlayRegistry();
  const closeA = reg.open(() => RECT_A);
  const closeB = reg.open(() => RECT_B);
  assert.equal(reg.openCount, 2);
  assert.deepEqual(reg.currentRects(), [RECT_A, RECT_B]);
  closeA();
  assert.equal(reg.isOpen, true, "still one open");
  assert.equal(reg.openCount, 1);
  assert.deepEqual(reg.currentRects(), [RECT_B]);
  closeB();
  assert.equal(reg.isOpen, false);
  assert.equal(reg.openCount, 0);
});

test("closing the same closer twice only removes it once", () => {
  const reg = new OverlayRegistry();
  const closeA = reg.open(() => RECT_A);
  reg.open(() => RECT_B);
  closeA();
  closeA(); // double-close (e.g. Escape racing a click) must not double-remove
  assert.equal(reg.openCount, 1);
});

test("currentRects() reads the getter live, not a snapshot from open() time", () => {
  const reg = new OverlayRegistry();
  let rect = { left: 0, top: 0, width: 10, height: 10 };
  reg.open(() => rect);
  assert.deepEqual(reg.currentRects(), [{ left: 0, top: 0, width: 10, height: 10 }]);
  rect = { left: 5, top: 5, width: 20, height: 20 }; // the overlay moved/resized while open
  assert.deepEqual(reg.currentRects(), [{ left: 5, top: 5, width: 20, height: 20 }]);
});

test("a getter returning null contributes nothing rather than throwing", () => {
  const reg = new OverlayRegistry();
  reg.open(() => null);
  reg.open(() => RECT_A);
  assert.equal(reg.openCount, 2, "still counted as open");
  assert.deepEqual(reg.currentRects(), [RECT_A]);
});

// #391 W3: a context menu is ONE overlay whose visible area is several disjoint
// boxes — `.ctxmenu-sub` is `position: absolute; left: 100%`, so it renders outside
// the root's border box and the root's own rect never covers it. Registering the
// root alone left submenu items over a plugin pane painted behind the native child
// webview and dead to the pointer, which is #391's reported symptom.
test("a getter may report SEVERAL rects — a menu root plus its open submenu — and all are reported", () => {
  const reg = new OverlayRegistry();
  const ROOT = { left: 100, top: 100, width: 190, height: 120 };
  const SUB = { left: 290, top: 95, width: 160, height: 60 }; // starts where ROOT ends
  reg.open(() => [ROOT, SUB]);
  assert.deepEqual(reg.currentRects(), [ROOT, SUB]);
});

test("a multi-rect overlay is still ONE open overlay, closed by its one closer", () => {
  const reg = new OverlayRegistry();
  const close = reg.open(() => [RECT_A, RECT_B]);
  assert.equal(reg.openCount, 1, "one menu, not one per panel");
  close();
  assert.deepEqual(reg.currentRects(), []);
});

// A closed submenu is `display: none` -> an all-zero rect at the viewport origin.
// Left in, a pane whose own rect starts at (0,0) would take a phantom hole from it;
// dropped here, a call site can return its whole set unconditionally instead of
// guessing at each sub-box's computed visibility.
test("an empty rect contributes nothing — a closed submenu measures all-zero", () => {
  const reg = new OverlayRegistry();
  const CLOSED_SUB = { left: 0, top: 0, width: 0, height: 0 };
  reg.open(() => [RECT_A, CLOSED_SUB]);
  assert.deepEqual(reg.currentRects(), [RECT_A]);
});

test("an empty rect from a single-rect getter is dropped too", () => {
  const reg = new OverlayRegistry();
  reg.open(() => ({ left: 4, top: 4, width: 0, height: 12 }));
  assert.equal(reg.openCount, 1, "still counted as open");
  assert.deepEqual(reg.currentRects(), []);
});

test("an empty array contributes nothing and does not throw", () => {
  const reg = new OverlayRegistry();
  reg.open(() => []);
  reg.open(() => RECT_A);
  assert.equal(reg.openCount, 2);
  assert.deepEqual(reg.currentRects(), [RECT_A]);
});

// End to end through the two modules that decide what the native HWND clip is:
// the registry's rect set -> pluginocclusion's pane-local holes. This is the shape
// #391 is actually about — "the submenu's items are clickable over a plugin pane"
// reduces to "a second hole was punched where the submenu is".
test("a menu with an open submenu over a plugin pane punches a hole for BOTH panels", () => {
  const reg = new OverlayRegistry();
  const pane = { left: 0, top: 0, width: 800, height: 600 }; // a plugin pane filling the window
  const root = { left: 100, top: 100, width: 190, height: 120 };
  const sub = { left: 290, top: 95, width: 160, height: 60 };
  reg.open(() => [root, sub]);

  const holes = computeExcludeRects(pane, reg.currentRects());
  assert.deepEqual(holes, [
    { x: 100, y: 100, width: 190, height: 120 },
    { x: 290, y: 95, width: 160, height: 60 },
  ]);
});

test("subscribe fires on every open/close edge", () => {
  const reg = new OverlayRegistry();
  let calls = 0;
  reg.subscribe(() => calls++);
  const close = reg.open(() => RECT_A);
  reg.open(() => RECT_B);
  close();
  assert.equal(calls, 3);
});

test("poke() fires subscribers without an open/close edge", () => {
  const reg = new OverlayRegistry();
  let calls = 0;
  reg.subscribe(() => calls++);
  reg.poke();
  assert.equal(calls, 1);
});

// #380: subscribers need to tell WHY they were notified (PluginPaneView's
// breadcrumb trigger-source label distinguishes an overlay opening from one
// closing) — open()/the closer/poke() each carry their own reason through.
test("subscribe's callback receives the reason for each edge", () => {
  const reg = new OverlayRegistry();
  const reasons: string[] = [];
  reg.subscribe((reason) => reasons.push(reason));
  const close = reg.open(() => RECT_A);
  reg.poke();
  close();
  assert.deepEqual(reasons, ["open", "poke", "close"]);
});

test("unsubscribe stops further notifications", () => {
  const reg = new OverlayRegistry();
  let calls = 0;
  const unsub = reg.subscribe(() => calls++);
  reg.open(() => RECT_A);
  unsub();
  reg.open(() => RECT_B);
  assert.equal(calls, 1);
});

test("separate instances don't share state", () => {
  const a = new OverlayRegistry();
  const b = new OverlayRegistry();
  a.open(() => RECT_A);
  assert.equal(a.isOpen, true);
  assert.equal(b.isOpen, false);
});
