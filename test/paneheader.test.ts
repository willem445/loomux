// Unit tests for the pane header's overflow policy (#2191, #2335). Run with `npm test`.
//
// Every input `planHeaderFit` reads gets its own varying fixture: the header's
// width, the fixed chrome's width, each control's width, how many controls there
// are, which of them are priority, the overflow button's width, the rung the
// header is currently on, and the three tunables (gap, title floor, hysteresis).
// A fixture that
// does not DISCRIMINATE pins nothing, so each of these asserts that moving the
// axis flips (or provably does not flip) the plan.
import { test } from "node:test";
import assert from "node:assert/strict";
import type { HeaderControl, HeaderFitInput, HeaderFitStage } from "../src/paneheader.ts";
import {
  planHeaderFit,
  menuLeftFor,
  overflowMenuIds,
  RENAME_ENTRY_ID,
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
  stage: "full",
};

// Derived by hand from BASE and re-checked by `the arithmetic behind the fixture`
// below, so a change to any default reddens there rather than silently shifting
// every threshold in this file.
const FULL_W = 359; // 100 fixed + 56 title floor + 7 * (23 + 6)
const UNFOLD_AT = FULL_W + HYSTERESIS_W; // 383
// The three rungs below it (#2335), each derived the same way and re-checked by
// `the ladder's four rungs` below.
const FOLDED_W = 243; // 100 fixed + 56 title floor + 2 priority * 29 + (23 + 6) for ⋯
const SQUEEZED_W = 187; // the same row with the title floor released
const MINIMAL_W = 129; // 100 fixed + (23 + 6) for ⋯, alone

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
  assert.equal(fit({ headerWidth: between, stage: "full" }).folded, false);
  assert.equal(fit({ headerWidth: between, stage: "folded" }).folded, true);
});

test("hysteresis: the unfold threshold sits a full dead band above the fold one", () => {
  assert.equal(fit({ headerWidth: UNFOLD_AT - 1, stage: "folded" }).folded, true);
  assert.equal(fit({ headerWidth: UNFOLD_AT, stage: "folded" }).folded, false);
});

test("hysteresis: a width oscillating inside the dead band never flaps", () => {
  // A divider dragged back and forth across the fold point: feed each width the
  // state the previous width produced, and the set must settle, not strobe.
  let stage: HeaderFitStage = "folded";
  const seen = new Set<HeaderFitStage>();
  for (const w of [FULL_W + 2, FULL_W + 20, FULL_W + 5, FULL_W + 22, FULL_W + 1]) {
    stage = planHeaderFit({ ...BASE, headerWidth: w, stage }).stage;
    seen.add(stage);
  }
  assert.deepEqual([...seen], ["folded"], "a width inside the dead band must not change the state");

  // Control: the SAME walk from the unfolded state also never flaps, but settles
  // on the other value — so the assertion above is about the dead band, not about
  // `folded` being ignored.
  let unfolded: HeaderFitStage = "full";
  for (const w of [FULL_W + 2, FULL_W + 20, FULL_W + 5, FULL_W + 22, FULL_W + 1]) {
    unfolded = planHeaderFit({ ...BASE, headerWidth: w, stage: unfolded }).stage;
  }
  assert.equal(unfolded, "full");
});

test("hysteresis: zero dead band collapses the two thresholds onto each other", () => {
  const between = FULL_W + 10;
  assert.equal(fit({ headerWidth: between, stage: "folded", hysteresis: 0 }).folded, false);
  assert.equal(fit({ headerWidth: between, stage: "folded", hysteresis: 24 }).folded, true);
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

test("a header whose controls are ALL priority does not fold above the last rung", () => {
  // "However narrow" was #2191's wording and is no longer true: the terminal
  // rung folds the priority set too, which is what #2335 is for. Everything
  // above that rung is unchanged, and this is the width that says so.
  const allPriority = CONTROLS.map((c) => ctl(c.id, c.width, true));
  const plan = fit({ headerWidth: 320, controls: allPriority });
  assert.equal(plan.folded, false);
  assert.deepEqual(plan.overflow, []);
  // Discriminating half: the same width with the same controls, one of them
  // demoted, DOES fold — so the pass above is the priority flag, not the width.
  // Demoted WIDE, because a 23px control swapped for a 23px overflow button is
  // the wash the next test pins; this axis is the flag, not the arithmetic.
  const oneFoldable = allPriority.map((c) => (c.id === "git" ? ctl("git", 60, false) : c));
  assert.equal(fit({ headerWidth: 320, controls: oneFoldable }).folded, true);
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
    for (const stage of ["full", "folded", "squeezed", "minimal"] as const) {
      const plan = planHeaderFit({ ...BASE, headerWidth, stage });
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
  assert.equal(fit({ headerWidth: 200, controls: one, overflowWidth: 40 }).folded, false);
  // Discriminating half: the same single foldable with a CHEAPER overflow button
  // does fold.
  assert.equal(fit({ headerWidth: 200, controls: one, overflowWidth: 10 }).folded, true);
});

test("an overflow button exactly as wide as the set it replaces is refused (a wash)", () => {
  const one = [ctl("git", 23), ctl("min", 23, true)];
  assert.equal(fit({ headerWidth: 200, controls: one, overflowWidth: 23 }).folded, false);
  assert.equal(fit({ headerWidth: 200, controls: one, overflowWidth: 22 }).folded, true);
});

// -------------------------------------------------------- not-yet-laid-out box

test("a header with no measurable width carries the current state, either way", () => {
  for (const headerWidth of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.equal(fit({ headerWidth, stage: "full" }).folded, false, `w=${headerWidth} unfolded`);
    assert.equal(fit({ headerWidth, stage: "folded" }).folded, true, `w=${headerWidth} folded`);
  }
  // The carried folded state is a real plan, not an empty one.
  const plan = fit({ headerWidth: 0, stage: "folded" });
  assert.deepEqual(plan.inline, ["min", "max"]);
  assert.deepEqual(plan.overflow, ["git", "issues", "editor", "split", "close"]);
});

test("nothing-to-fold beats the not-laid-out carry: never a button over an empty menu", () => {
  // Guard ORDER, not just guard presence. A pane with no foldable control that
  // is somehow already marked folded (a kind change while its tab was hidden)
  // must come back unfolded rather than carry a state whose menu holds nothing.
  const allPriority = CONTROLS.map((c) => ctl(c.id, c.width, true));
  const plan = fit({ headerWidth: 0, stage: "folded", controls: allPriority });
  assert.equal(plan.folded, false);
  assert.deepEqual(plan.overflow, []);
  assert.deepEqual(plan.inline, CONTROLS.map((c) => c.id));
  // Same for a pane with no controls at all.
  assert.deepEqual(fit({ headerWidth: 0, stage: "folded", controls: [] }), {
    stage: "full",
    folded: false,
    inline: [],
    overflow: [],
    title: "floor",
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

// ------------------------------------------------------------- the fold ladder
// #2335. Below the #2191 fold there are two more rungs, and the reason there are
// any is that the old ladder stopped here: at minimum width the row still held
// ⋯ + minimize + maximize, all three overflowed the box, and `.pane`'s
// `overflow: hidden` clipped every one of them — a header with no reachable
// control at all. Each test below pins one step of the priority order.

test("the ladder's four rungs are the thresholds the fixture's arithmetic predicts", () => {
  // Same role as `the arithmetic behind the fixture` above, for the two new
  // rungs: every literal in this section is derived here once.
  assert.equal(FOLDED_W, BASE.fixedWidth + TITLE_MIN_W + 2 * (23 + HEADER_GAP_W) + (23 + HEADER_GAP_W));
  assert.equal(SQUEEZED_W, FOLDED_W - TITLE_MIN_W);
  assert.equal(MINIMAL_W, BASE.fixedWidth + (23 + HEADER_GAP_W));
  // Strictly decreasing — a rung that is not narrower than the one above is not
  // on the ladder at all, which is the guard the welcome-pane test pins.
  assert.ok(FULL_W > FOLDED_W && FOLDED_W > SQUEEZED_W && SQUEEZED_W > MINIMAL_W);
});

test("the fold order at decreasing widths is full → folded → squeezed → minimal", () => {
  // One walk down, each rung read at its own threshold and one pixel below it.
  // Every field of the plan is asserted, because the rungs differ in different
  // fields: `folded` separates 1 from 2, `title` separates 2 from 3, and
  // `inline` separates 3 from 4.
  assert.deepEqual(fit({ headerWidth: FULL_W }), {
    stage: "full",
    folded: false,
    inline: ["git", "issues", "editor", "split", "close", "min", "max"],
    overflow: [],
    title: "floor",
  });
  assert.deepEqual(fit({ headerWidth: FULL_W - 1 }), {
    stage: "folded",
    folded: true,
    inline: ["min", "max"],
    overflow: ["git", "issues", "editor", "split", "close"],
    title: "floor",
  });
  assert.deepEqual(fit({ headerWidth: FOLDED_W }).stage, "folded");
  assert.deepEqual(fit({ headerWidth: FOLDED_W - 1 }), {
    stage: "squeezed",
    folded: true,
    inline: ["min", "max"],
    overflow: ["git", "issues", "editor", "split", "close"],
    title: "shrink",
  });
  assert.deepEqual(fit({ headerWidth: SQUEEZED_W }).stage, "squeezed");
  assert.deepEqual(fit({ headerWidth: SQUEEZED_W - 1 }), {
    stage: "minimal",
    folded: true,
    inline: [],
    overflow: ["git", "issues", "editor", "split", "close", "min", "max"],
    title: "hidden",
  });
});

test("the pane name is truncated BEFORE a priority control folds", () => {
  // The order the issue asks for in as many words: the ⋯ button has priority
  // over the title, so the title's floor is spent first — there is a whole band
  // of widths where the name has given way and minimize/maximize have not.
  const band: number[] = [];
  for (let w = MINIMAL_W; w <= FULL_W; w++) {
    const plan = fit({ headerWidth: w });
    if (plan.title === "shrink" && plan.inline.length > 0) band.push(w);
  }
  assert.ok(band.length > 0, "no width truncates the name while keeping a control inline");
  // ...and that band sits strictly ABOVE every width that folds the cluster, so
  // the two steps are ordered rather than merely both present.
  const collapsed: number[] = [];
  for (let w = MINIMAL_W; w <= FULL_W; w++) {
    if (fit({ headerWidth: w }).inline.length === 0) collapsed.push(w);
  }
  assert.ok(collapsed.length > 0, "no width folds the priority cluster");
  assert.ok(Math.min(...band) > Math.max(...collapsed));
});

test("at the narrowest width EVERY control is in the menu and ⋯ is the row", () => {
  // The terminal state the issue names: minimize, maximize and the rest are all
  // in the menu, the name is out of the row, and the one thing left is the
  // button that opens the menu.
  const plan = fit({ headerWidth: 1 });
  assert.equal(plan.stage, "minimal");
  assert.equal(plan.folded, true, "the ⋯ button must be in the row");
  assert.deepEqual(plan.inline, []);
  assert.deepEqual(plan.overflow, CONTROLS.map((c) => c.id));
  assert.equal(plan.title, "hidden");
  // Discriminating half: the same call one rung up keeps the cluster inline, so
  // this is the width, not the policy having only one answer.
  assert.deepEqual(fit({ headerWidth: SQUEEZED_W }).inline, ["min", "max"]);
});

test("no width leaves the header with nothing to click", () => {
  // The bug, stated as the property that forbids it. Swept rather than sampled,
  // because the failure was at ONE end of the range and a sample of three widths
  // is exactly what missed it.
  const reachable: string[] = [];
  for (let w = 0; w <= FULL_W + 200; w++) {
    const plan = fit({ headerWidth: w });
    if (!plan.folded && plan.inline.length === 0) reachable.push(`w=${w}`);
  }
  assert.deepEqual(reachable, [], "these widths render neither a control nor the ⋯ button");
  // Positive control: the sweep really did read plans at both ends of the range,
  // so an empty result is a clean sweep and not an empty one.
  assert.equal(fit({ headerWidth: 0 }).folded, false, "w=0 carries the unfolded BASE stage");
  assert.equal(fit({ headerWidth: 1 }).folded, true);
  assert.equal(fit({ headerWidth: FULL_W + 200 }).folded, false);
});

// ------------------------------------------------------------- the menu's rows

test("the menu carries the pane name only at the rung where the row has lost it", () => {
  const minimal = fit({ headerWidth: 1 });
  assert.deepEqual(overflowMenuIds(minimal), [
    RENAME_ENTRY_ID,
    "git",
    "issues",
    "editor",
    "split",
    "close",
    "min",
    "max",
  ]);
  // One rung up the name is still in the row, so the menu must NOT offer it —
  // two routes to the same thing on a header that has room for one.
  const squeezed = fit({ headerWidth: SQUEEZED_W });
  assert.equal(squeezed.title, "shrink");
  assert.deepEqual(overflowMenuIds(squeezed), squeezed.overflow);
  assert.ok(!overflowMenuIds(squeezed).includes(RENAME_ENTRY_ID));
});

test("a wide header folds nothing and offers an empty menu (negative control)", () => {
  const plan = fit({ headerWidth: 800 });
  assert.equal(plan.stage, "full");
  assert.equal(plan.folded, false);
  assert.equal(plan.title, "floor");
  assert.deepEqual(plan.overflow, []);
  assert.deepEqual(overflowMenuIds(plan), []);
  // ...and it stays that way from the folding threshold upwards, so "wide" is a
  // band rather than the one width that happened to be sampled.
  for (const w of [FULL_W, FULL_W + 1, FULL_W + 500, 4000]) {
    assert.deepEqual(overflowMenuIds(fit({ headerWidth: w })), [], `width ${w}`);
  }
});

// -------------------------------------------------- rungs that buy nothing

test("a welcome pane's single ✕ stays inline at EVERY width — it never folds", () => {
  // `OVERFLOW_BTN_W` is deliberately wider than `.pane-btn`, so replacing one
  // button with ⋯ costs room instead of saving it. Both folding rungs are
  // therefore off this pane's ladder, and the narrowest state it can reach still
  // has its ✕ in the row (#2191's guarantee, which #2335's new rung must not
  // quietly repeal).
  const welcome = [ctl("close", 23)];
  for (const headerWidth of [1, 50, MINIMAL_W, SQUEEZED_W, 400]) {
    const plan = fit({ headerWidth, controls: welcome, overflowWidth: 25 });
    assert.equal(plan.folded, false, `width ${headerWidth}`);
    assert.deepEqual(plan.inline, ["close"], `width ${headerWidth}`);
    assert.deepEqual(plan.overflow, [], `width ${headerWidth}`);
  }
  // Discriminating half: a ⋯ button CHEAPER than the button it replaces puts
  // both rungs back on the ladder, so the pass above is the arithmetic and not
  // "a one-control header is special-cased".
  const cheap = fit({ headerWidth: 1, controls: welcome, overflowWidth: 10 });
  assert.equal(cheap.folded, true);
  assert.deepEqual(cheap.overflow, ["close"]);
});

test("an all-priority header still collapses at the narrowest rung", () => {
  // #2191 refused to fold a header with nothing foldable, at every width. That
  // is still right one rung up — folding buys nothing there — but it is exactly
  // what left minimize and maximize unreachable at minimum width, so the last
  // rung folds the priority set too.
  const allPriority = CONTROLS.map((c) => ctl(c.id, c.width, true));
  const wide = fit({ headerWidth: 320, controls: allPriority });
  assert.equal(wide.folded, false, "an intermediate width must not fold a priority-only set");
  assert.deepEqual(wide.overflow, []);
  assert.equal(wide.title, "shrink", "the name gives way first, exactly as elsewhere");

  const narrow = fit({ headerWidth: 100, controls: allPriority });
  assert.equal(narrow.folded, true);
  assert.deepEqual(narrow.inline, []);
  assert.deepEqual(narrow.overflow, CONTROLS.map((c) => c.id));
});

// -------------------------------------------------- hysteresis across the rungs

test("hysteresis: coming back UP from minimal needs spare room, going down does not", () => {
  // The dead band is a property of the ladder, not of the one #2191 threshold.
  assert.equal(fit({ headerWidth: SQUEEZED_W, stage: "squeezed" }).stage, "squeezed");
  assert.equal(fit({ headerWidth: SQUEEZED_W, stage: "minimal" }).stage, "minimal");
  assert.equal(
    fit({ headerWidth: SQUEEZED_W + HYSTERESIS_W, stage: "minimal" }).stage,
    "squeezed"
  );
  // Falling is immediate in both directions of travel — a row that no longer
  // fits must not wait for a dead band before shedding something.
  assert.equal(fit({ headerWidth: SQUEEZED_W - 1, stage: "squeezed" }).stage, "minimal");
});

test("hysteresis: a width oscillating around the minimal threshold never flaps", () => {
  let stage: HeaderFitStage = "minimal";
  const seen = new Set<HeaderFitStage>();
  for (const w of [SQUEEZED_W + 2, SQUEEZED_W + 20, SQUEEZED_W + 5, SQUEEZED_W + 1]) {
    stage = planHeaderFit({ ...BASE, headerWidth: w, stage }).stage;
    seen.add(stage);
  }
  assert.deepEqual([...seen], ["minimal"]);
  // Control: the same walk entered from `squeezed` settles on the other value,
  // so the assertion above is the dead band and not `stage` being ignored.
  let from: HeaderFitStage = "squeezed";
  for (const w of [SQUEEZED_W + 2, SQUEEZED_W + 20, SQUEEZED_W + 5, SQUEEZED_W + 1]) {
    from = planHeaderFit({ ...BASE, headerWidth: w, stage: from }).stage;
  }
  assert.equal(from, "squeezed");
});

test("an unlaid-out header carries EVERY rung, including the two new ones", () => {
  for (const stage of ["full", "folded", "squeezed", "minimal"] as const) {
    assert.equal(fit({ headerWidth: 0, stage }).stage, stage, `carry ${stage}`);
  }
  // The carried minimal state is a real plan, not an empty one.
  const plan = fit({ headerWidth: 0, stage: "minimal" });
  assert.deepEqual(plan.inline, []);
  assert.deepEqual(plan.overflow, CONTROLS.map((c) => c.id));
  assert.equal(plan.title, "hidden");
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
