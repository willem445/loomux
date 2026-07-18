// The shared "which DOM overlays are open, and where" registry (#391, folded
// into #380). DOM-free — no overlay actually opens here, just the
// tracking/notify contract every overlay call site and PluginPaneView rely
// on. `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { OverlayRegistry } from "../src/overlaystate.ts";

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
