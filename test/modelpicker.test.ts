// Pins the ModelPicker contract the repo field depends on (#2108): the
// initial-focus marker re-homing from #2010's review and the branch-flip
// round trip. The class is DOM-bound — its decisions are attribute wiring
// between its two halves — so these tests drive the REAL ModelPicker through
// a minimal element shim, the same instrument rev-final used to verify
// #2104. The shim stays tiny: only what the class touches.
//
// Red-before-green: the custom…-pick, its focusWelcome resolution and the
// round-trip tests fail against #2104's pre-fix blob `ca0e4a46` of
// src/modelpicker.ts — there the predicate read `sel.hidden`, which nothing
// in the repo ever writes, so the marker could never come back to the input.
import { test } from "node:test";
import assert from "node:assert/strict";
import { CUSTOM_OPTION } from "../src/modelcatalog.ts";

// --- minimal DOM shim -------------------------------------------------------
// Only what ModelPicker touches. The one fidelity point that matters is the
// <select> value semantics: a select whose value names no option reads ""
// (selectedIndex −1), while an input reads back what was written.
class ShimElement {
  tagName: string;
  className = "";
  hidden = false;
  textContent = "";
  placeholder = "";
  spellcheck = false;
  children: ShimElement[] = [];
  options: ShimElement[] = [];
  listeners = new Map<string, (() => void)[]>();
  attrs = new Map<string, string>();
  private storedValue = "";

  constructor(tag: string) {
    this.tagName = tag.toUpperCase();
  }

  addEventListener(type: string, fn: () => void): void {
    const list = this.listeners.get(type) ?? [];
    list.push(fn);
    this.listeners.set(type, list);
  }

  append(...kids: ShimElement[]): void {
    this.children.push(...kids);
  }

  replaceChildren(...kids: ShimElement[]): void {
    this.options = [...kids];
    this.children = [...kids];
  }

  appendChild(kid: ShimElement): ShimElement {
    this.options.push(kid);
    this.children.push(kid);
    return kid;
  }

  getAttribute(name: string): string | null {
    return this.attrs.get(name) ?? null;
  }
  setAttribute(name: string, v: string): void {
    this.attrs.set(name, v);
  }
  removeAttribute(name: string): void {
    this.attrs.delete(name);
  }
  hasAttribute(name: string): boolean {
    return this.attrs.has(name);
  }

  // No-op: focus resolution is asserted through the marker (see
  // focusWelcomeTarget below), the way pane.ts's focusWelcome reads it.
  focus(): void {}

  get value(): string {
    if (this.tagName !== "SELECT") return this.storedValue;
    return this.options.some((o) => o.value === this.storedValue) ? this.storedValue : "";
  }
  set value(v: string) {
    this.storedValue = v;
  }
}

// Installed before the dynamic import below; the constructor runs at test
// time and nothing in the module graph touches `document` before that.
(globalThis as Record<string, unknown>).document = {
  createElement: (tag: string) => new ShimElement(tag),
};

const { ModelPicker } = await import("../src/modelpicker.ts");

type Picker = InstanceType<typeof ModelPicker>;

function fire(select: ShimElement, type: string): void {
  for (const fn of select.listeners.get(type) ?? []) fn();
}

const MARKER = "data-initial-focus";
type Half = "select" | "input";

function markedHalf(picker: Picker): Half | null {
  if (picker.input.hasAttribute(MARKER)) return "input";
  if (picker.select.hasAttribute(MARKER)) return "select";
  return null;
}

// Models pane.ts's focusWelcome: the FIRST [data-initial-focus] in DOM order,
// and the picker's root appends select, input, summary in that order.
function focusWelcomeTarget(picker: Picker): Half | null {
  const first = (picker.root as unknown as ShimElement).children.find((el) =>
    el.hasAttribute(MARKER)
  );
  if (first === (picker.select as unknown as ShimElement)) return "select";
  if (first === (picker.input as unknown as ShimElement)) return "input";
  return null;
}

// Mirrors the launcher's seed (launcher.ts:917→923): setOptions decides the
// branch, then the host stamps the marker on whichever half is visible.
function makePicker(recents: string[], fallback: string): Picker {
  const picker = new ModelPicker();
  picker.setOptions(recents, fallback);
  (picker.input.hidden ? picker.select : picker.input).setAttribute(MARKER, "");
  return picker;
}

// A real user flip: choosing "custom…" in the dropdown sets the value, then
// fires change — the class's own listener does the rest.
function pickCustom(picker: Picker): void {
  (picker.select as unknown as ShimElement).value = CUSTOM_OPTION;
  fire(picker.select as unknown as ShimElement, "change");
}

// --- the contract under test ------------------------------------------------

test("the seed stamps the visible half: the select on the dropdown branch", () => {
  const p = makePicker(["C:\\a", "C:\\b"], "C:\\a");
  assert.equal(p.input.hidden, true, "a value that IS a recent opens the dropdown branch");
  assert.equal(markedHalf(p), "select");
});

test("the seed stamps the visible half: the input on the custom branch", () => {
  const p = makePicker(["C:\\a", "C:\\b"], "C:\\not-a-recent");
  assert.equal(p.input.hidden, false, "an unknown path opens the custom branch");
  assert.equal(markedHalf(p), "input");
});

test("picking custom… re-homes the marker to the free-text input", () => {
  const p = makePicker(["C:\\a", "C:\\b"], "C:\\a");
  pickCustom(p);
  assert.equal(p.input.hidden, false);
  assert.equal(markedHalf(p), "input");
});

test("after the custom… pick focusWelcome resolves to the input", () => {
  // A stranded marker on the select is not merely a dead end: a select is a
  // value-changing control, one arrow key fires change and silently replaces
  // a half-typed path with a recent directory.
  const p = makePicker(["C:\\a", "C:\\b"], "C:\\a");
  pickCustom(p);
  assert.equal(focusWelcomeTarget(p), "input");
});

test("a Browse… pick of a known recent re-homes the marker to the select", () => {
  const p = makePicker(["C:\\a", "C:\\b"], "C:\\not-a-recent");
  p.value = "C:\\a";
  assert.equal(p.input.hidden, true);
  assert.equal(markedHalf(p), "select");
  assert.equal(focusWelcomeTarget(p), "select");
});

test("when both halves are showing the input carries the marker", () => {
  // The custom branch shows both halves (the class never hides the select),
  // and the input wins: it is the half a human is typing into.
  const p = makePicker(["C:\\a", "C:\\b"], "C:\\a");
  pickCustom(p);
  assert.equal(p.input.hidden, false);
  assert.equal(markedHalf(p), "input");
});

test("the marker round-trips dropdown ↔ custom in both directions", () => {
  const p = makePicker(["C:\\a", "C:\\b"], "C:\\a");
  pickCustom(p);
  assert.equal(markedHalf(p), "input", "flip 1: dropdown → custom");
  p.value = "C:\\b";
  assert.equal(markedHalf(p), "select", "flip 2: custom → dropdown (Browse… onto a recent)");
  pickCustom(p);
  assert.equal(markedHalf(p), "input", "flip 3: dropdown → custom again");
  p.value = "C:\\a";
  assert.equal(markedHalf(p), "select", "flip 4: custom → dropdown again");
});

test("a pane whose marker is not on the picker gains none from a branch flip", () => {
  // No marker anywhere → the picker must not invent one; a marker it did not
  // stamp is never moved.
  const p = makePicker(["C:\\a"], "C:\\a");
  p.select.removeAttribute(MARKER);
  pickCustom(p);
  assert.equal(markedHalf(p), null);
});

// --- #2108 item 2: setOptions is a branch-flip site too ---------------------

test("setOptions re-homes the marker when a rebuild flips to the custom branch", () => {
  // Recents changed underneath the control (a launch recorded a new
  // directory): the seeded value is no longer on the list and the fallback is
  // not a recent either, so the rebuild opens the custom branch.
  const p = makePicker(["C:\\a", "C:\\b"], "C:\\a");
  assert.equal(markedHalf(p), "select");
  p.setOptions(["C:\\x", "C:\\y"], "C:\\new");
  assert.equal(p.input.hidden, false);
  assert.equal(markedHalf(p), "input");
});

test("setOptions re-homes the marker when a rebuild flips to the dropdown branch", () => {
  // The custom branch's own value becoming a recent flips the rebuild to the
  // dropdown branch, hiding the input under the marker.
  const p = makePicker(["C:\\a", "C:\\b"], "C:\\typed-path");
  assert.equal(markedHalf(p), "input");
  p.setOptions(["C:\\typed-path", "C:\\a"], "C:\\a");
  assert.equal(p.input.hidden, true);
  assert.equal(markedHalf(p), "select");
});

// --- #2108 item 3: stale custom text ----------------------------------------

test("a value set that takes the dropdown branch clears the stale custom text", () => {
  const p = makePicker(["C:\\a", "C:\\b"], "C:\\not-a-recent");
  (p.input as unknown as ShimElement).value = "C:\\half-typed";
  p.value = "C:\\a";
  assert.equal(p.input.value, "", "the input is hidden by this branch; its old text must not survive");
  assert.equal(p.value, "C:\\a");
  // Flipping back to custom… shows an empty box, not the stale path.
  pickCustom(p);
  assert.equal(p.input.value, "");
});

test("a value set that takes the custom branch still carries the value", () => {
  const p = makePicker(["C:\\a", "C:\\b"], "C:\\a");
  p.value = "C:\\elsewhere";
  assert.equal(p.input.value, "C:\\elsewhere");
  assert.equal(p.value, "C:\\elsewhere");
});
