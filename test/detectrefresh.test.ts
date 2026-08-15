// A detection reply refreshes every surface it invalidates — on BOTH hosts
// (#993, #997 review blocking 1).
//
// STRUCTURAL, and read off the source tree on purpose. `detect` is a DOM-wiring
// path, and this repo validates DOM wiring by hand rather than by simulating a
// DOM (CLAUDE.md, code conventions) — so the thing that went wrong here cannot
// be reached by a behavioural test at all. What went wrong was a *missing call*:
// the workflow block editor repainted the model dropdown and nothing else, while
// its sibling reply handler four hundred lines up re-ran the analysis pass and
// repainted the knob rows. The data was fresh the whole time; the DOM was stale.
//
// The failure that produces is not cosmetic. `knobLookup` answers differently
// once a reply lands, so the Thinking-level row goes on offering `xhigh` for a
// model whose reply says it has no effort setting — the human picks it, the next
// mutation re-renders the row disabled, and the findings pane flags the block.
// The editor offered a value its own validator rejects, which is the exact thing
// `workflowview.ts`'s own comment claims cannot happen.
//
// So this is a source scan, in the tradition of `transport.test.ts`'s
// one-module-imports-@tauri-apps rule and `tests/groupid.rs`'s two scans: the
// property is "this handler reaches these refreshers", the next one will be
// written by copy-paste from a neighbour rather than by reading the rule, and a
// scan is what notices.
//
// **What this instrument does NOT do**, stated here rather than left for a
// reader to discover, the way `groupid.rs`'s scans enumerate their own blind
// spots:
//
//   - It asserts REACHABILITY BY NAME, not behaviour. A handler that calls
//     `renderFindings()` on a path that never runs still passes. The one piece
//     of this feature with real logic in it — `runWhenNotEditing` — is pinned on
//     behaviour instead, at the bottom of this file.
//   - It reads the SOURCE, not the module graph, so a refresher reached through
//     an indirection it cannot see reads as absent. That is the safe direction:
//     it under-recognises rather than over-claiming.
//   - It assumes each `onDetect:` is an arrow function and asserts so, rather
//     than silently scanning past a shape it does not understand.
//
// It covers EVERY `onDetect:` in each file, not just the first (#997 review
// NB-4). There is one per host today, so first-only was sound as written — which
// is exactly why it was worth fixing: a second picker would have gone unchecked
// while the suite stayed green, and a scan that silently covers a subset is
// worse than no scan, because it reads as coverage.
//
// The vacuity guards below are load-bearing, not decoration: they fail if the
// region this thinks it is scanning stops being one. They caught two bugs in the
// scanner itself before this file was correct.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const src = (file: string): string =>
  readFileSync(fileURLToPath(new URL(`../src/${file}`, import.meta.url)), "utf8");

/** The source of the `onDetect:` handler starting at `at`.
 *
 *  Balanced-delimiter scan rather than a line count or a regex: the handler is a
 *  multi-line arrow inside an object literal, and either of the cheap
 *  alternatives would quietly capture the wrong region the moment somebody
 *  reformats — which is the failure mode that makes a structural test worse than
 *  no test. Throws rather than returning `""` if the marker is missing, so a
 *  renamed hook fails loudly instead of passing vacuously. */
function onDetectBodyAt(text: string, at: number, where: string): string {
  // Start counting AFTER the arrow's parameter list: `() =>` opens and closes a
  // pair immediately, so a scan that began at `onDetect:` would decide the
  // handler ended before its body began. (It did, on the first run of this
  // test — the vacuity guard below is what caught it, which is the argument for
  // having one.)
  const arrow = text.indexOf("=>", at);
  assert.notEqual(arrow, -1, `${where}: \`onDetect:\` is not an arrow function — this scan assumes it is`);
  // Run to the end of the object-literal PROPERTY, not to the first balanced
  // pair: one host's handler is a chained expression
  // (`detect(cli).then(...)`), whose `(cli)` returns the depth to zero long
  // before the body is over. The property ends at a top-level `,`, or at the
  // `}` that closes the object it sits in.
  let depth = 0;
  for (let i = arrow; i < text.length; i += 1) {
    const c = text[i];
    if (c === "(" || c === "{" || c === "[") {
      depth += 1;
    } else if (c === ")" || c === "}" || c === "]") {
      if (depth === 0) return text.slice(at, i);
      depth -= 1;
    } else if (c === "," && depth === 0) {
      return text.slice(at, i);
    }
  }
  throw new Error(`${where}: unbalanced delimiters after \`onDetect:\` — the scan could not find the end of the handler`);
}

/** EVERY `onDetect:` handler in `text`, not just the first (#997 review NB-4).
 *
 *  There is exactly one per host today, so scanning only the first was sound as
 *  written — and that is precisely the reason to fix it rather than note it: a
 *  second picker on either surface would go entirely unchecked while the suite
 *  stayed green, which is the failure mode a structural test has to be honest
 *  about. A scan that silently covers a subset is worse than one that covers
 *  nothing, because it reads as coverage.
 *
 *  Asserts at least one, so a renamed hook fails loudly instead of vacuously
 *  passing over an empty list — the same reason every assertion here has a
 *  vacuity guard. */
function onDetectBodies(text: string, where: string): string[] {
  const bodies: string[] = [];
  for (let at = text.indexOf("onDetect:"); at !== -1; at = text.indexOf("onDetect:", at + 1)) {
    bodies.push(onDetectBodyAt(text, at, where));
  }
  assert.notEqual(
    bodies.length,
    0,
    `${where} no longer has an \`onDetect:\` hook — this scan is looking at the wrong thing`
  );
  return bodies;
}

test("the workflow block editor refreshes the knobs and the findings after a detection", () => {
  for (const body of onDetectBodies(src("workflowview.ts"), "workflowview.ts")) {
    // Vacuity guard, and it is the `detect(` match that does the work: if the
    // scan ever extracts the wrong region, every assertion below would pass or
    // fail for reasons that have nothing to do with the rule. Deliberately NOT a
    // generous length threshold — the buggy handler this test exists to catch was
    // a single short line, so a length gate tuned to reject it would fire first
    // and report "the scan is broken" instead of the finding.
    assert.match(body, /modelCatalog\.detect\(/, "the handler under test must be the one that asks the CLI");
    assert.ok(body.length > 40, `the extracted handler is implausibly short, so this test is not reading it: ${body}`);

    // The three the sibling `agent_cli_knobs` handler does, and the ones the first
    // cut of this handler did none of.
    assert.match(
      body,
      /analyzeWorkflow\(/,
      "a detection changes what `knobLookup` answers, so the analysis pass owes a re-run — otherwise the findings " +
        "pane disagrees with the controls until an unrelated edit happens to re-run it"
    );
    assert.match(body, /renderFindings\(/, "…and the findings it just recomputed have to be painted");
    assert.match(
      body,
      /repaintBlockKnobs\?\.\(\)/,
      "the knob repaint must go through the LIVE `this.repaintBlockKnobs?.()`, never a captured closure: " +
        "`renderForm()` nulls it precisely so a late reply cannot paint into a row it has already detached"
    );
  }
});

test("a detection that outlives its form does not paint that form's rows", () => {
  // #997 review NB-1. An ask spawns a CLI and can be in flight for seconds —
  // long enough to select another block, at which point `renderForm()` has
  // detached these rows. The handler has to notice before it touches any of
  // this form's DOM, the same way the probe reply eleven lines below it does.
  for (const body of onDetectBodies(src("workflowview.ts"), "workflowview.ts")) {
    assert.match(
      body,
      /formPane\.contains\(picker\.root\)/,
      "the handler must check its own form is still on screen before repainting it — otherwise a reply that lands " +
        "after the human moved on paints a detached row, and the form they are actually looking at stays stale"
    );
  }
});

test("the launcher refreshes its knob row after a detection, by either branch", () => {
  for (const body of onDetectBodies(src("launcher.ts"), "launcher.ts")) {
    assert.match(body, /catalog\.detect\(/, "the handler under test must be the one that asks the CLI");
    assert.ok(body.length > 40, `the extracted handler is implausibly short, so this test is not reading it: ${body}`);

    // The launcher reaches the knobs either directly (when a mid-type guard stops
    // it rebuilding the menu) or through `applyRoleModels`. Both are acceptable;
    // reaching NEITHER is the bug.
    assert.match(
      body,
      /applyRoleKnobs\(|applyRoleModels\(/,
      "a launcher detection must reach the knob repaint by one branch or the other"
    );
  }
});

test("the launcher's model repaint really does carry the knob repaint with it", () => {
  // The half of the chain the scan above takes on trust. Pinned here rather than
  // assumed, because `applyRoleModels` is what makes the launcher's shorter
  // handler equivalent to the workflow pane's longer one — if that call were
  // ever dropped, the launcher test above would still pass while the launcher
  // had the workflow pane's bug.
  const text = src("launcher.ts");
  const at = text.indexOf("private applyRoleModels(");
  assert.notEqual(at, -1, "`applyRoleModels` was renamed — the launcher scan's assumption needs revisiting");
  const body = text.slice(at, text.indexOf("\n  }", at));
  assert.match(body, /applyRoleKnobs\(/, "applyRoleModels is only a valid knob-refresh route while it calls one");
});

test("both hosts DEFER the mid-type menu rebuild rather than dropping it", () => {
  // #997 review NB-3 (round 2), then NB-3 again (round 3). Round 2 got the first
  // half: an ask can be in flight for seconds — long enough for a human to click
  // into the `custom…` box — and rebuilding the menu under a half-typed id hides
  // the input beneath the caret. Round 2 got the second half WRONG: it dropped
  // the rebuild instead of postponing it, which on the launcher was permanent —
  // `applyRoleModels` is otherwise reachable only from the CLI `change` listener
  // and the seed pass, so the human saw a `detect` click do nothing at all for
  // the rest of the dialog's life.
  //
  // So the pin is on `runWhenNotEditing`, not on `editingCustom`: guarding is
  // necessary but not sufficient, and the bare predicate is what the dropped
  // version also used.
  for (const file of ["workflowview.ts", "launcher.ts"]) {
    for (const body of onDetectBodies(src(file), file)) {
      assert.match(
        body,
        /runWhenNotEditing\(/,
        `${file}'s detect handler must DEFER the menu rebuild past the mid-type window, not drop it — a guard that ` +
          `only refuses is a detect click that silently does nothing`
      );
    }
  }
});

test("the deferral really is a deferral — it runs the rebuild, not just a guard", () => {
  // The half the source scans above cannot see. `runWhenNotEditing` is the one
  // piece of this fix with real logic in it, so it is pinned on behaviour rather
  // than on reachability: it must invoke the rebuild in both states, and the
  // whole point of round 3 is that the mid-type state defers rather than
  // swallows.
  const text = src("modelpicker.ts");
  const at = text.indexOf("runWhenNotEditing(");
  assert.notEqual(at, -1, "`runWhenNotEditing` was renamed — the host scans above are asserting a name that is gone");
  const body = text.slice(at, text.indexOf("\n  }", at));
  assert.match(body, /rebuild\(\)/, "the not-editing path must actually run the rebuild");
  assert.match(
    body,
    /addEventListener\("blur"[\s\S]*rebuild\(\)/,
    "and the editing path must schedule it for when the hazard ends, rather than returning without it"
  );
  assert.match(body, /once: true/, "one deferral, one rebuild — a listener left attached would fire on every later blur");
});
