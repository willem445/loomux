// Unit tests for the pane header's overflow policy (#2191). Run with `npm test`.
//
// Every input `planHeaderFit` reads gets its own varying fixture: the header's
// width, the fixed chrome's width, each control's width, how many controls there
// are, which of them are priority, the overflow button's width, the current fold
// state, and the three tunables (gap, title floor, hysteresis). A fixture that
// does not DISCRIMINATE pins nothing, so each of these asserts that moving the
// axis flips (or provably does not flip) the plan.
import { test } from "node:test";
import assert from "node:assert/strict";
import type { HeaderControl, HeaderFitInput } from "../src/paneheader.ts";
import {
  planHeaderFit,
  menuLeftFor,
  HEADER_GAP_W,
  TITLE_MIN_W,
  HYSTERESIS_W,
  CONTROL_W_FALLBACK,
  MENU_EDGE_PAD,
} from "../src/paneheader.ts";

const ctl = (id: string, width = 23, priority = false): HeaderControl => ({ id, width, priority });

/** A plain terminal pane's header: five foldable icons, then the two priority
 *  window controls. Widths at `.pane-btn`'s own 23px. */
const CONTROLS: HeaderControl[] = [
  ctl("git"),
  ctl("issues"),
  ctl("editor"),
  ctl("split"),
  ctl("close"),
  ctl("min", 23, true),
  ctl("max", 23, true),
];

const BASE: HeaderFitInput = {
  headerWidth: 800,
  fixedWidth: 100,
  controls: CONTROLS,
  overflowWidth: 23,
  folded: false,
};

// Derived by hand from BASE and re-checked by `the arithmetic behind the fixture`
// below, so a change to any default reddens there rather than silently shifting
// every threshold in this file.
const FULL_W = 359; // 100 fixed + 56 title floor + 7 * (23 + 6)
const UNFOLD_AT = FULL_W + HYSTERESIS_W; // 383

const fit = (over: Partial<HeaderFitInput> = {}) => planHeaderFit({ ...BASE, ...over });

test("the arithmetic behind the fixture is what the defaults actually produce", () => {
  // Not decoration: every threshold below is written as a literal offset from
  // FULL_W, so this is the one place the defaults are tied to those literals.
  assert.equal(HEADER_GAP_W, 6);
  assert.equal(TITLE_MIN_W, 56);
  assert.equal(HYSTERESIS_W, 24);
  const expected =
    BASE.fixedWidth + TITLE_MIN_W + CONTROLS.length * (23 + HEADER_GAP_W);
  assert.equal(expected, FULL_W);
  // ...and FULL_W really is the fold threshold for an unfolded header.
  assert.equal(fit({ headerWidth: FULL_W }).folded, false);
  assert.equal(fit({ headerWidth: FULL_W - 1 }).folded, true);
});

// ---------------------------------------------------------------- header width

test("a wide header keeps every control inline, in header order", () => {
  const plan = fit({ headerWidth: 800 });
  assert.equal(plan.folded, false);
  assert.deepEqual(plan.inline, ["git", "issues", "editor", "split", "close", "min", "max"]);
  assert.deepEqual(plan.overflow, []);
});

test("a narrow header folds every non-priority control, in header order", () => {
  const plan = fit({ headerWidth: 200 });
  assert.equal(plan.folded, true);
  assert.deepEqual(plan.inline, ["min", "max"]);
  assert.deepEqual(plan.overflow, ["git", "issues", "editor", "split", "close"]);
});

test("the fold threshold is exactly 'the full set no longer fits'", () => {
  assert.equal(fit({ headerWidth: FULL_W + 1 }).folded, false);
  assert.equal(fit({ headerWidth: FULL_W }).folded, false);
  assert.equal(fit({ headerWidth: FULL_W - 1 }).folded, true);
});

// ------------------------------------------------------------------ hysteresis

test("hysteresis: an already-folded header needs SPARE room before it unfolds", () => {
  // The same width decides differently depending on where we came from — which is
  // the whole point, and is not true of any other input.
  const between = FULL_W + 10; // inside the dead band [FULL_W, FULL_W + HYSTERESIS_W)
  assert.equal(fit({ headerWidth: between, folded: false }).folded, false);
  assert.equal(fit({ headerWidth: between, folded: true }).folded, true);
});

test("hysteresis: the unfold threshold sits a full dead band above the fold one", () => {
  assert.equal(fit({ headerWidth: UNFOLD_AT - 1, folded: true }).folded, true);
  assert.equal(fit({ headerWidth: UNFOLD_AT, folded: true }).folded, false);
});

test("hysteresis: a width oscillating inside the dead band never flaps", () => {
  // A divider dragged back and forth across the fold point: feed each width the
  // state the previous width produced, and the set must settle, not strobe.
  let folded = true;
  const seen = new Set<boolean>();
  for (const w of [FULL_W + 2, FULL_W + 20, FULL_W + 5, FULL_W + 22, FULL_W + 1]) {
    folded = planHeaderFit({ ...BASE, headerWidth: w, folded }).folded;
    seen.add(folded);
  }
  assert.deepEqual([...seen], [true], "a width inside the dead band must not change the state");

  // Control: the SAME walk from the unfolded state also never flaps, but settles
  // on the other value — so the assertion above is about the dead band, not about
  // `folded` being ignored.
  let unfolded = false;
  for (const w of [FULL_W + 2, FULL_W + 20, FULL_W + 5, FULL_W + 22, FULL_W + 1]) {
    unfolded = planHeaderFit({ ...BASE, headerWidth: w, folded: unfolded }).folded;
  }
  assert.equal(unfolded, false);
});

test("hysteresis: zero dead band collapses the two thresholds onto each other", () => {
  const between = FULL_W + 10;
  assert.equal(fit({ headerWidth: between, folded: true, hysteresis: 0 }).folded, false);
  assert.equal(fit({ headerWidth: between, folded: true, hysteresis: 24 }).folded, true);
});

// ---------------------------------------------------------------- fixed chrome

test("wider fixed chrome (chips, folder/branch) folds at a wider header", () => {
  const w = FULL_W + 40;
  assert.equal(fit({ headerWidth: w }).folded, false);
  assert.equal(fit({ headerWidth: w, fixedWidth: BASE.fixedWidth + 41 }).folded, true);
});

// -------------------------------------------------------------- control widths

test("one fat control folds a header that the same count of thin ones fits", () => {
  const w = FULL_W;
  assert.equal(fit({ headerWidth: w }).folded, false);
  const fat = CONTROLS.map((c) => (c.id === "git" ? ctl("git", 60) : c));
  assert.equal(fit({ headerWidth: w, controls: fat }).folded, true);
});

test("an unmeasurable control width falls back to the .pane-btn width", () => {
  const unmeasured = CONTROLS.map((c) => (c.id === "git" ? ctl("git", Number.NaN) : c));
  // NaN is read as CONTROL_W_FALLBACK (23), which is what "git" already was — so
  // the threshold is unmoved. Discriminating half: a control genuinely narrower
  // than the fallback DOES move it, proving the fallback is a substitution and
  // not "any odd width becomes 23".
  assert.equal(fit({ headerWidth: FULL_W, controls: unmeasured }).folded, false);
  assert.equal(fit({ headerWidth: FULL_W - 1, controls: unmeasured }).folded, true);
  assert.equal(CONTROL_W_FALLBACK, 23);
  const thin = CONTROLS.map((c) => (c.id === "git" ? ctl("git", 3) : c));
  assert.equal(fit({ headerWidth: FULL_W - 1, controls: thin }).folded, false);
});

// --------------------------------------------------------------- control count

test("more controls fold a header that fewer ones fit", () => {
  const w = FULL_W;
  assert.equal(fit({ headerWidth: w }).folded, false);
  assert.equal(fit({ headerWidth: w, controls: [...CONTROLS, ctl("extra")] }).folded, true);
});

test("fewer controls unfold a header that the full set folds", () => {
  const w = FULL_W - 30;
  assert.equal(fit({ headerWidth: w }).folded, true);
  assert.equal(fit({ headerWidth: w, controls: CONTROLS.slice(2) }).folded, false);
});

// -------------------------------------------------------------- priority flags

test("a header whose controls are ALL priority never folds, however narrow", () => {
  const allPriority = CONTROLS.map((c) => ctl(c.id, c.width, true));
  const plan = fit({ headerWidth: 10, controls: allPriority });
  assert.equal(plan.folded, false);
  assert.deepEqual(plan.overflow, []);
  // Discriminating half: the same width with the same controls, one of them
  // demoted, DOES fold — so the pass above is the priority flag, not the width.
  // Demoted WIDE, because a 23px control swapped for a 23px overflow button is
  // the wash the next test pins; this axis is the flag, not the arithmetic.
  const oneFoldable = allPriority.map((c) => (c.id === "git" ? ctl("git", 60, false) : c));
  assert.equal(fit({ headerWidth: 10, controls: oneFoldable }).folded, true);
});

test("which controls are priority decides what survives inline", () => {
  const flipped = CONTROLS.map((c) =>
    ctl(c.id, c.width, c.id === "close" || c.id === "git")
  );
  const plan = fit({ headerWidth: 200, controls: flipped });
  assert.equal(plan.folded, true);
  assert.deepEqual(plan.inline, ["git", "close"]);
  assert.deepEqual(plan.overflow, ["issues", "editor", "split", "min", "max"]);
});

test("inline and overflow together are exactly the controls, with nothing duplicated", () => {
  for (const headerWidth of [10, 200, FULL_W - 1, FULL_W, 800]) {
    for (const folded of [false, true]) {
      const plan = planHeaderFit({ ...BASE, headerWidth, folded });
      const all = [...plan.inline, ...plan.overflow].sort();
      assert.deepEqual(all, CONTROLS.map((c) => c.id).sort(), `width ${headerWidth}`);
      if (!plan.folded) assert.deepEqual(plan.overflow, []);
    }
  }
});

// ------------------------------------------------------------- overflow button

test("folding is refused when the overflow button costs more than it saves", () => {
  // One foldable control: swapping it for a WIDER overflow button hides a control
  // and buys nothing.
  const one = [ctl("git", 20), ctl("min", 23, true), ctl("max", 23, true)];
  assert.equal(fit({ headerWidth: 10, controls: one, overflowWidth: 40 }).folded, false);
  // Discriminating half: the same single foldable with a CHEAPER overflow button
  // does fold.
  assert.equal(fit({ headerWidth: 10, controls: one, overflowWidth: 10 }).folded, true);
});

test("an overflow button exactly as wide as the set it replaces is refused (a wash)", () => {
  const one = [ctl("git", 23), ctl("min", 23, true)];
  assert.equal(fit({ headerWidth: 10, controls: one, overflowWidth: 23 }).folded, false);
  assert.equal(fit({ headerWidth: 10, controls: one, overflowWidth: 22 }).folded, true);
});

// -------------------------------------------------------- not-yet-laid-out box

test("a header with no measurable width carries the current state, either way", () => {
  for (const headerWidth of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.equal(fit({ headerWidth, folded: false }).folded, false, `w=${headerWidth} unfolded`);
    assert.equal(fit({ headerWidth, folded: true }).folded, true, `w=${headerWidth} folded`);
  }
  // The carried folded state is a real plan, not an empty one.
  const plan = fit({ headerWidth: 0, folded: true });
  assert.deepEqual(plan.inline, ["min", "max"]);
  assert.deepEqual(plan.overflow, ["git", "issues", "editor", "split", "close"]);
});

test("nothing-to-fold beats the not-laid-out carry: never a button over an empty menu", () => {
  // Guard ORDER, not just guard presence. A pane with no foldable control that
  // is somehow already marked folded (a kind change while its tab was hidden)
  // must come back unfolded rather than carry a state whose menu holds nothing.
  const allPriority = CONTROLS.map((c) => ctl(c.id, c.width, true));
  const plan = fit({ headerWidth: 0, folded: true, controls: allPriority });
  assert.equal(plan.folded, false);
  assert.deepEqual(plan.overflow, []);
  assert.deepEqual(plan.inline, CONTROLS.map((c) => c.id));
  // Same for a pane with no controls at all.
  assert.deepEqual(fit({ headerWidth: 0, folded: true, controls: [] }), {
    folded: false,
    inline: [],
    overflow: [],
  });
});

// ----------------------------------------------------------------- gap / title

test("a bigger inter-item gap folds a header that a tighter one fits", () => {
  assert.equal(fit({ headerWidth: FULL_W }).folded, false);
  assert.equal(fit({ headerWidth: FULL_W, gap: 12 }).folded, true);
  // ...and a tighter gap unfolds one the default folds.
  assert.equal(fit({ headerWidth: FULL_W - 20 }).folded, true);
  assert.equal(fit({ headerWidth: FULL_W - 20, gap: 2 }).folded, false);
});

test("a bigger title floor folds a header that a smaller one fits", () => {
  assert.equal(fit({ headerWidth: FULL_W }).folded, false);
  assert.equal(fit({ headerWidth: FULL_W, titleMinWidth: TITLE_MIN_W + 1 }).folded, true);
  assert.equal(fit({ headerWidth: FULL_W - 30, titleMinWidth: 20 }).folded, false);
});

// -------------------------------------------------------------- menu placement

test("a menu with room stays left-aligned with its trigger", () => {
  assert.equal(menuLeftFor(100, 123, 200, 600), 100);
});

test("a menu whose trigger sits near the right edge flips to right-aligned", () => {
  // Left-aligned would put the menu at 560..760 in a 600px box; flipped it is
  // 383..583, entirely inside.
  assert.equal(menuLeftFor(560, 583, 200, 600), 583 - 200);
});

test("even the flipped menu is clamped inside the container", () => {
  // Trigger overhangs the box (a rounding artefact at the very edge): the flip
  // alone would still leave the menu past `containerWidth - pad`.
  assert.equal(menuLeftFor(590, 613, 200, 600), 600 - 200 - MENU_EDGE_PAD);
});

test("a menu is never placed left of the edge padding", () => {
  assert.equal(menuLeftFor(0, 23, 200, 600), MENU_EDGE_PAD);
  assert.equal(menuLeftFor(-40, -17, 200, 600), MENU_EDGE_PAD);
});

test("a menu at least as wide as its container goes flush to the left edge", () => {
  assert.equal(menuLeftFor(10, 33, 700, 600), 0);
  // The boundary: a menu that leaves less than 2*pad of slack has nowhere to be
  // padded, so it starts at `pad` rather than being pushed negative.
  assert.equal(menuLeftFor(10, 33, 600 - MENU_EDGE_PAD, 600), MENU_EDGE_PAD);
});

test("menu placement tolerates unmeasured boxes rather than emitting NaN", () => {
  for (const args of [
    [Number.NaN, Number.NaN, 200, 600],
    [100, 123, Number.NaN, 600],
    [100, 123, 200, Number.NaN],
    [100, 123, 200, 0],
  ] as const) {
    const left = menuLeftFor(args[0], args[1], args[2], args[3]);
    assert.ok(Number.isFinite(left), `menuLeftFor(${args.join(", ")}) must be finite`);
    assert.ok(left >= 0, `menuLeftFor(${args.join(", ")}) must not be negative`);
  }
});
