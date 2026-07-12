// `hidden` means hidden — the stylesheet invariant the workflow pane's live bug was made of (#222).
//
// This is a test about CSS, which is unusual here and deliberate. The workflow pane's surface rules
// (workflowpane.ts) were CORRECT and unit-tested, and `render()` obeyed them exactly: one surface,
// `hidden` on the other two. The pane still showed all three at once in the real app — an error
// banner over a workflow that had loaded and validated, and a live "Create workflow" button that
// then scaffolded over that workflow — because `hidden` doesn't hide anything the stylesheet has
// given a `display` to. Author declarations outrank the UA stylesheet's `[hidden] { display: none }`
// by ORIGIN, before specificity is even consulted. The bug lived in the one place the pane's tests
// could never look.
//
// WHAT GUARANTEES THE FIX, AND WHAT THESE TESTS ACTUALLY CHECK (rev-17 F2). The fix is one
// declaration — `[hidden] { display: none !important }` — and its strength comes from `!important`,
// not from its selector:
//
//   * An IMPORTANT author declaration beats every NORMAL author declaration. Full stop. Specificity
//     and source order are only consulted between declarations of equal importance, so a normal
//     `display` rule cannot defeat the guard no matter how baroque its selector — `#a .b > .c:hover`
//     loses to it exactly as `.wf-body` does. That is a proof from the cascade, not an assumption,
//     and it is why the compound-selector model below is allowed to skip complex selectors: what it
//     skips has already lost.
//   * The ONE thing that can defeat the guard is ANOTHER IMPORTANT `display` declaration, which
//     ties on importance and then wins on specificity or source order. Nothing in the cascade stops
//     it, and no model of compound selectors would see it coming if it were written as a descendant.
//
// So the guarantee is split in two, and both halves are tested: `the guard is the only important
// display in the stylesheet` (which needs no cascade model at all — it reads every declaration in
// the file, whatever its selector, and fails loudly on any it cannot rule out), and `hiding an
// element hides it` (which models the cascade over the compound selectors that actually carry
// `display` here, and catches the guard being weakened or deleted).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

// Comments out first — this file's comments are prose ABOUT selectors and declarations, and a
// parser that reads them as either would be reading the argument instead of the code.
const CSS = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8").replace(
  /\/\*[\s\S]*?\*\//g,
  ""
);

interface Rule {
  selector: string;
  display: string;
  important: boolean;
  /** Source order — later wins at equal weight, which is the trap `!important` closes. */
  order: number;
}

/** Every `display` declaration in the stylesheet, whatever its selector.
 *
 *  The `[^{}]*` body cannot cross a brace, so this matches INNERMOST blocks only — which means the
 *  rules nested inside `@media` are read (with their real selectors) and the at-rule header itself
 *  is simply never matched as a rule. Nothing carrying a `display` is skipped. */
function displayRules(css: string): Rule[] {
  const rules: Rule[] = [];
  let order = 0;
  for (const block of css.matchAll(/([^{}]*)\{([^{}]*)\}/g)) {
    const selector = block[1].trim();
    order += 1;
    for (const decl of block[2].split(";")) {
      const m = /^\s*display\s*:\s*([^!]+?)\s*(!\s*important)?\s*$/.exec(decl);
      if (!m) continue;
      rules.push({ selector, display: m[1].trim(), important: !!m[2], order });
    }
  }
  return rules;
}

const RULES = displayRules(CSS);

/** The guard itself: bare `[hidden]`, `display: none`, important. */
const isGuard = (r: Rule): boolean =>
  r.selector === "[hidden]" && r.display === "none" && r.important;

test("the guard exists: `hidden` is enforced with `!important`, not left to source order", () => {
  const guards = RULES.filter(isGuard);
  assert.equal(guards.length, 1, "styles.css must declare `[hidden] { display: none !important }`");
  // Without `!important` this whole file is decoration: an author-origin `[hidden]` ties on
  // specificity with any single class, so the winner would be decided by SOURCE ORDER — i.e. by
  // whether the next person to write `display:` writes it above or below the guard.
});

test("the guard is the ONLY important `display` in the stylesheet — nothing can out-rank it", () => {
  // THE HOLE THIS CLOSES (rev-17 F2). The cascade model below reads compound selectors, so an
  // important `display` hung on a DESCENDANT selector (`.pane .wf-body { display: flex !important }`)
  // would defeat the guard while every other test in this file stayed green. This one cannot miss
  // it: it reads every declaration in the file, whatever the selector, and admits exactly one.
  //
  // It is deliberately absolute rather than clever — any new important `display`, even
  // `display: none`, fails here. That is the point: an important `display` is the only kind of rule
  // that can interact with the guard at all, so it should never be added without someone reading
  // this comment and reasoning it through against a hidden element.
  const offenders = RULES.filter((r) => r.important && !isGuard(r)).map(
    (r) => `${r.selector} { display: ${r.display} !important }`
  );
  assert.deepEqual(
    offenders,
    [],
    `only \`[hidden]\` may declare an important \`display\` — these tie with the guard and can beat ` +
      `it on specificity or source order, so \`el.hidden = true\` would silently stop working: ` +
      offenders.join("; ")
  );
});

// ---------- the cascade, over the selectors that actually carry `display` here ----------

/** A `display` rule whose selector is a SIMPLE COMPOUND (`.a`, `.a.b`, `.a[hidden]`) — the only
 *  shape whose cascade can be modelled honestly without a real CSS engine, and the shape this
 *  stylesheet uses for every `display` it puts on a class.
 *
 *  Skipping the rest is sound, and not a blind spot: everything skipped here is a NORMAL
 *  declaration (the test above admits no other important one), and a normal declaration loses to
 *  the important guard regardless of its selector. What is skipped has already lost. */
interface Compound {
  classes: string[];
  needsHidden: boolean;
  display: string;
  important: boolean;
  order: number;
}

const COMPOUNDS: Compound[] = RULES.flatMap((r) =>
  r.selector
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s && !/[\s>+~:]/.test(s))
    .flatMap((sel) => {
      const classes = [...sel.matchAll(/\.([\w-]+)/g)].map((m) => m[1]);
      const needsHidden = /\[hidden\]/.test(sel);
      if (!classes.length && !needsHidden) return []; // ids and tags — never toggled by class
      return [{ classes, needsHidden, display: r.display, important: r.important, order: r.order }];
    })
);

/** What `display` an element with these classes and `hidden` actually computes to.
 *
 *  The cascade, in the only three steps that matter here: important author declarations beat normal
 *  ones; among equals, higher specificity wins; among THOSE equals, source order does. The UA's
 *  `[hidden] { display: none }` is the floor, and it loses to any author declaration at all — which
 *  is the whole reason this file exists. */
function computedDisplay(classes: string[], hidden: boolean): string {
  const has = new Set(classes);
  const matched = COMPOUNDS.filter(
    (d) => (!d.needsHidden || hidden) && d.classes.every((c) => has.has(c))
  );
  if (!matched.length) return hidden ? "none" : "block"; // UA floor
  const specificity = (d: Compound): number => d.classes.length + (d.needsHidden ? 1 : 0);
  const winner = matched.reduce((best, d) => {
    if (d.important !== best.important) return d.important ? d : best;
    if (specificity(d) !== specificity(best)) return specificity(d) > specificity(best) ? d : best;
    return d.order > best.order ? d : best;
  });
  return winner.display;
}

test("hiding an element hides it — no class rule may out-rank the `hidden` attribute", () => {
  // THE INVARIANT, stated over the whole stylesheet rather than the seven elements that happened to
  // get bitten. Every class that is given a `display` is a class that some future `el.hidden = true`
  // will be quietly ignored on; enumerating today's victims would just mean rediscovering this the
  // next time somebody writes `display: flex`.
  const styled = new Set(
    COMPOUNDS.filter((d) => !d.needsHidden && d.display !== "none").flatMap((d) => d.classes)
  );
  const defiant = [...styled].filter((c) => computedDisplay([c], true) !== "none");
  assert.deepEqual(
    defiant,
    [],
    `these classes stay on screen after \`el.hidden = true\`: ${defiant.slice(0, 8).join(", ")}` +
      `${defiant.length > 8 ? ` (+${defiant.length - 8} more)` : ""}`
  );
});

test("the workflow pane's surfaces are mutually exclusive ON SCREEN, not just in the rules", () => {
  // The live bug, named. `render()` picks ONE surface (paneSurface) and sets `hidden` on the other
  // two — and the human still saw the error surface, the start surface AND a loaded workflow stacked
  // in one pane, because all three are `display: flex`. The rules were never the problem.
  for (const cls of ["wf-start", "wf-body", "wf-findings"]) {
    assert.equal(
      computedDisplay([cls], true),
      "none",
      `.${cls} is a workflow-pane surface — hiding it must hide it`
    );
  }

  // Same for the tabs, which had the same defect and would have shown the YAML and the graph stacked
  // under the form: `applyTab()` hides two of the three the same way.
  for (const cls of ["wf-yaml", "wf-graph"]) {
    assert.equal(computedDisplay([cls], true), "none", `.${cls} is a tab pane`);
  }

  // …and the view's own root, which is how `hide()` is supposed to work at all.
  assert.equal(computedDisplay(["wf"], true), "none");
});

test("the two elements outside the pane that were hiding wrong are hidden now too", () => {
  // Found by sweeping the app for elements toggled via `.hidden` whose class carries a `display`
  // (rev-17 F1 — the first sweep missed the second of these, and said so in a PR body). Both had
  // been quietly ignoring their own `hidden` since the day they were written:
  //
  //   .group-auto-meter — groupview's budget meter, whose own comment reads `Off ⇒ hidden`.
  //   .tab-close        — the tab ✕, hidden on a single tab (`never zero tabs`). Visible, it was a
  //                       DEAD control: `TabBar.requestClose` floors at `count <= 1` and returns.
  //
  // They are pinned here because they are the app's whole population of this bug, and the next one
  // to join them should have to walk past a test that says so.
  assert.equal(computedDisplay(["group-auto-meter"], true), "none");
  assert.equal(computedDisplay(["tab-close"], true), "none");
});

test("a surface that is NOT hidden still lays out as it was designed to", () => {
  // The other half, and the reason the fix is `[hidden]`-scoped rather than dropping the `display`
  // rules: the surfaces are flex containers and must stay flex containers. A guard that fixed the
  // bug by making the pane's layout collapse would be a worse bug wearing a passing test.
  assert.equal(computedDisplay(["wf-body"], false), "flex");
  assert.equal(computedDisplay(["wf-start"], false), "flex");
  assert.equal(computedDisplay(["wf-findings"], false), "flex");
  assert.equal(computedDisplay(["tab-close"], false), "inline-flex");
  assert.equal(computedDisplay(["group-auto-meter"], false), "inline-flex");
});
