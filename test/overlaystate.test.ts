// The shared "an overlay is open" registry (#391). DOM-free — no overlay
// actually opens here, just the counting/notify contract every overlay call
// site and PluginPaneView rely on. `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { OverlayRegistry } from "../src/overlaystate.ts";

test("starts closed", () => {
  const reg = new OverlayRegistry();
  assert.equal(reg.isOpen, false);
  assert.equal(reg.openCount, 0);
});

test("open() marks it open", () => {
  const reg = new OverlayRegistry();
  reg.open();
  assert.equal(reg.isOpen, true);
  assert.equal(reg.openCount, 1);
});

test("the returned closer closes it", () => {
  const reg = new OverlayRegistry();
  const close = reg.open();
  close();
  assert.equal(reg.isOpen, false);
  assert.equal(reg.openCount, 0);
});

test("two overlays open at once — only the LAST close reports closed", () => {
  const reg = new OverlayRegistry();
  const closeA = reg.open();
  const closeB = reg.open();
  assert.equal(reg.openCount, 2);
  closeA();
  assert.equal(reg.isOpen, true, "still one open");
  assert.equal(reg.openCount, 1);
  closeB();
  assert.equal(reg.isOpen, false);
  assert.equal(reg.openCount, 0);
});

test("closing the same closer twice only decrements once", () => {
  const reg = new OverlayRegistry();
  const closeA = reg.open();
  reg.open();
  closeA();
  closeA(); // double-close (e.g. Escape racing a click) must not double-decrement
  assert.equal(reg.openCount, 1);
});

test("count never goes negative", () => {
  const reg = new OverlayRegistry();
  const close = reg.open();
  close();
  close();
  assert.equal(reg.openCount, 0);
});

test("subscribe is called with the new count on every open/close", () => {
  const reg = new OverlayRegistry();
  const seen: number[] = [];
  reg.subscribe((n) => seen.push(n));
  const close = reg.open();
  reg.open();
  close();
  assert.deepEqual(seen, [1, 2, 1]);
});

test("unsubscribe stops further notifications", () => {
  const reg = new OverlayRegistry();
  const seen: number[] = [];
  const unsub = reg.subscribe((n) => seen.push(n));
  reg.open();
  unsub();
  reg.open();
  assert.deepEqual(seen, [1]);
});

test("separate instances don't share state", () => {
  const a = new OverlayRegistry();
  const b = new OverlayRegistry();
  a.open();
  assert.equal(a.isOpen, true);
  assert.equal(b.isOpen, false);
});
