// `hidden` means hidden — the stylesheet invariant the workflow pane's live bug was made of (#222).
//
// This is a test about CSS, which is unusual here and deliberate. The workflow pane's surface
// rules (workflowpane.ts) were CORRECT and unit-tested, and `render()` obeyed them exactly: one
// surface, `hidden` on the other two. The pane still showed all three at once in the real app —
// an error banner over a workflow that had loaded and validated, and a live "Create workflow"
// button that then scaffolded over that workflow — because `hidden` doesn't hide anything the
// stylesheet has given a `display` to. Author declarations outrank the UA stylesheet's
// `[hidden] { display: none }` by ORIGIN, before specificity is even consulted.
//
// So the bug lived in the one place the pane's tests could never look, and no test of the rules,
// the model or the wiring would ever have found it. This one models the cascade and asks the
// question that matters: if the code hides an element, does the element go away?
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

// Comments out first — this file's comments are prose ABOUT selectors and declarations, and a
// parser that reads them as either would be reading the argument instead of the code.
const CSS = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8").replace(
  /\/\*[\s\S]*?\*\//g,
  ""
);

interface Decl {
  /** The classes the selector demands (all must be on the element). */
  classes: string[];
  /** Does the selector demand the `hidden` attribute? */
  needsHidden: boolean;
  display: string;
  important: boolean;
  /** Source order — later wins at equal weight, which is the trap `!important` closes. */
  order: number;
}

/** Every `display:` declaration on a SIMPLE COMPOUND selector (`.a`, `.a.b`, `.a[hidden]`).
 *
 *  Compounds are all this stylesheet uses to set `display`, and they are the only shape whose
 *  cascade can be modelled honestly without a real CSS engine. That is not a hole: the fix is an
 *  `!important` author declaration, and an important author rule beats every *normal* author rule
 *  regardless of how complicated its selector is. Anything this parser skips is a normal rule, and
 *  therefore already lost. */
function displayDecls(css: string): Decl[] {
  const decls: Decl[] = [];
  let order = 0;
  for (const rule of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const body = rule[2];
    const d = /(?:^|;)\s*display\s*:\s*([^;!}]+)(!important)?/.exec(body);
    if (!d) continue;
    const value = d[1].trim();
    const important = /display\s*:[^;}]*!important/.test(body);
    for (const sel of rule[1].split(",").map((s) => s.trim())) {
      order += 1;
      if (/[\s>+~:]/.test(sel)) continue; // not a simple compound — see the docblock
      const classes = [...sel.matchAll(/\.([\w-]+)/g)].map((m) => m[1]);
      const needsHidden = /\[hidden\]/.test(sel);
      // A bare `[hidden]` selector (no class) is the global guard; a compound of classes is a
      // normal styling rule. Anything else (ids, tags, other attributes) isn't in play here.
      if (!classes.length && !needsHidden) continue;
      decls.push({ classes, needsHidden, display: value, important, order });
    }
  }
  return decls;
}

/** What `display` an element with these classes and `hidden` actually computes to.
 *
 *  The cascade, in the only three steps that matter here: important author declarations beat
 *  normal ones; among equals, higher specificity wins; among THOSE equals, source order does. The
 *  UA's `[hidden] { display: none }` is the floor, and it loses to any author declaration at all —
 *  which is the whole reason this file exists. */
function computedDisplay(decls: Decl[], classes: string[], hidden: boolean): string {
  const has = new Set(classes);
  const matched = decls.filter(
    (d) => (!d.needsHidden || hidden) && d.classes.every((c) => has.has(c))
  );
  if (!matched.length) return hidden ? "none" : "block"; // UA floor
  const specificity = (d: Decl): number => d.classes.length + (d.needsHidden ? 1 : 0);
  const winner = matched.reduce((best, d) => {
    if (d.important !== best.important) return d.important ? d : best;
    if (specificity(d) !== specificity(best)) return specificity(d) > specificity(best) ? d : best;
    return d.order > best.order ? d : best;
  });
  return winner.display;
}

const DECLS = displayDecls(CSS);

test("hiding an element hides it — no class rule may out-rank the `hidden` attribute", () => {
  // THE INVARIANT, stated over the whole stylesheet rather than the seven elements that happened
  // to get bitten. Every class that is given a `display` is a class that some future `el.hidden =
  // true` will be quietly ignored on; enumerating today's victims would just mean rediscovering
  // this the next time somebody writes `display: flex`.
  const styled = new Set(
    DECLS.filter((d) => !d.needsHidden && d.display !== "none").flatMap((d) => d.classes)
  );
  const defiant = [...styled].filter((c) => computedDisplay(DECLS, [c], true) !== "none");
  assert.deepEqual(
    defiant,
    [],
    `these classes stay on screen after \`el.hidden = true\`: ${defiant.slice(0, 8).join(", ")}` +
      `${defiant.length > 8 ? ` (+${defiant.length - 8} more)` : ""}`
  );
});

test("the workflow pane's surfaces are mutually exclusive ON SCREEN, not just in the rules", () => {
  // The live bug, named. `render()` picks ONE surface (paneSurface) and sets `hidden` on the other
  // two — and the human still saw the error surface, the start surface AND a loaded workflow
  // stacked in one pane, because all three are `display: flex`. The rules were never the problem.
  for (const cls of ["wf-start", "wf-body", "wf-findings"]) {
    assert.equal(
      computedDisplay(DECLS, [cls], true),
      "none",
      `.${cls} is a workflow-pane surface — hiding it must hide it`
    );
  }

  // Same for the tabs, which had the same defect and would have shown the YAML and the graph
  // stacked under the form: `applyTab()` hides two of the three the same way.
  for (const cls of ["wf-yaml", "wf-graph"]) {
    assert.equal(computedDisplay(DECLS, [cls], true), "none", `.${cls} is a tab pane`);
  }

  // …and the view's own root, which is how `hide()` is supposed to work at all.
  assert.equal(computedDisplay(DECLS, ["wf"], true), "none");
});

test("a surface that is NOT hidden still lays out as it was designed to", () => {
  // The other half, and the reason the fix is `[hidden]`-scoped rather than dropping the `display`
  // rules: the surfaces are flex containers and must stay flex containers. A guard that fixed the
  // bug by making the pane's layout collapse would be a worse bug wearing a passing test.
  assert.equal(computedDisplay(DECLS, ["wf-body"], false), "flex");
  assert.equal(computedDisplay(DECLS, ["wf-start"], false), "flex");
  assert.equal(computedDisplay(DECLS, ["wf-findings"], false), "flex");
});
