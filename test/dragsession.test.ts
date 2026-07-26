// Unit tests for the shared drag-session helper (#361 review finding: a
// drag that ends without a `mouseup` — Alt-Tab away mid-drag fires window
// `blur` instead — used to strand whatever state `mousedown` applied,
// worst-case leaving a docked view's list permanently invisible under
// `content-visibility: hidden`). Exercised against a plain fake event
// target (mirrors domutil.ts's narrow-interface pattern), no real DOM
// required. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { startDragSession, type DragEventTarget } from "../src/dragsession.ts";

class FakeTarget implements DragEventTarget {
  private listeners = new Map<string, Set<(e: any) => void>>();
  addEventListener(type: string, listener: (e: any) => void): void {
    if (!this.listeners.has(type)) this.listeners.set(type, new Set());
    this.listeners.get(type)!.add(listener);
  }
  removeEventListener(type: string, listener: (e: any) => void): void {
    this.listeners.get(type)?.delete(listener);
  }
  fire(type: string, e: any = {}): void {
    for (const l of [...(this.listeners.get(type) ?? [])]) l(e);
  }
  listenerCount(type: string): number {
    return this.listeners.get(type)?.size ?? 0;
  }
}

test("wires mousemove/mouseup/blur/keydown, all four, on start", () => {
  const target = new FakeTarget();
  startDragSession({ onMove: () => {}, onEnd: () => {} }, target);
  assert.equal(target.listenerCount("mousemove"), 1);
  assert.equal(target.listenerCount("mouseup"), 1);
  assert.equal(target.listenerCount("blur"), 1);
  assert.equal(target.listenerCount("keydown"), 1);
});

test("mouseup ends the session and removes every listener", () => {
  const target = new FakeTarget();
  let ends = 0;
  startDragSession({ onMove: () => {}, onEnd: () => ends++ }, target);
  target.fire("mouseup");
  assert.equal(ends, 1);
  assert.equal(target.listenerCount("mousemove"), 0);
  assert.equal(target.listenerCount("mouseup"), 0);
  assert.equal(target.listenerCount("blur"), 0);
  assert.equal(target.listenerCount("keydown"), 0);
});

test("a window blur mid-drag ends the session exactly like mouseup — the bug this fixes", () => {
  // The real-world case: Alt-Tab away with the mouse button still physically
  // down. No mouseup is ever delivered to a window that lost focus.
  const target = new FakeTarget();
  let ends = 0;
  startDragSession({ onMove: () => {}, onEnd: () => ends++ }, target);
  target.fire("blur");
  assert.equal(ends, 1);
  assert.equal(target.listenerCount("mousemove"), 0, "must not still be listening after blur ends it");
});

test("Escape ends the session; any other key does not", () => {
  const target = new FakeTarget();
  let ends = 0;
  startDragSession({ onMove: () => {}, onEnd: () => ends++ }, target);
  target.fire("keydown", { key: "Shift" });
  assert.equal(ends, 0);
  target.fire("keydown", { key: "Escape" });
  assert.equal(ends, 1);
});

test("onEnd fires exactly once even if multiple end signals arrive", () => {
  // E.g. a blur followed by a mouseup that still lands (some platforms
  // deliver both) — the second signal must be a no-op, not a double-fire.
  const target = new FakeTarget();
  let ends = 0;
  startDragSession({ onMove: () => {}, onEnd: () => ends++ }, target);
  target.fire("blur");
  target.fire("mouseup");
  target.fire("keydown", { key: "Escape" });
  assert.equal(ends, 1);
});

test("onMove fires for mousemove events before the session ends, never after", () => {
  const target = new FakeTarget();
  const moves: number[] = [];
  startDragSession({ onMove: (e) => moves.push(e.clientX), onEnd: () => {} }, target);
  target.fire("mousemove", { clientX: 1 });
  target.fire("mousemove", { clientX: 2 });
  target.fire("mouseup");
  target.fire("mousemove", { clientX: 3 }); // no listener left — must not reach onMove
  assert.deepEqual(moves, [1, 2]);
});
