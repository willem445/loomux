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
// scan is what notices. It deliberately asserts REACHABILITY BY NAME, not
// behaviour — see the vacuity guards below, which fail if the region it thinks
// it is scanning stops being one.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const src = (file: string): string =>
  readFileSync(fileURLToPath(new URL(`../src/${file}`, import.meta.url)), "utf8");

/** The source of the arrow function assigned to the FIRST `onDetect:` in `text`.
 *
 *  Balanced-delimiter scan rather than a line count or a regex: the handler is a
 *  multi-line arrow inside an object literal, and either of the cheap
 *  alternatives would quietly capture the wrong region the moment somebody
 *  reformats — which is the failure mode that makes a structural test worse than
 *  no test. Throws rather than returning `""` if the marker is missing, so a
 *  renamed hook fails loudly instead of passing vacuously. */
function onDetectBody(text: string, where: string): string {
  const at = text.indexOf("onDetect:");
  assert.notEqual(at, -1, `${where} no longer has an \`onDetect:\` hook — this scan is looking at the wrong thing`);
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

test("the workflow block editor refreshes the knobs and the findings after a detection", () => {
  const body = onDetectBody(src("workflowview.ts"), "workflowview.ts");

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
    /repaintKnobs\(\)|repaintBlockKnobs/,
    "the Thinking-level row is what a detection is the answer FOR: leaving it stale is the editor offering effort " +
      "levels its own validator rejects"
  );
});

test("the launcher refreshes its knob row after a detection, by either branch", () => {
  const body = onDetectBody(src("launcher.ts"), "launcher.ts");

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

test("both hosts guard the same mid-type hazard the same way", () => {
  // #997 review, non-blocking 3: the two surfaces reasoned about one hazard
  // differently. An ask can be in flight for seconds — long enough for a human
  // to click into the `custom…` box — and rebuilding the menu under a half-typed
  // id hides the input beneath the caret.
  for (const file of ["workflowview.ts", "launcher.ts"]) {
    const body = onDetectBody(src(file), file);
    assert.match(
      body,
      /editingCustom/,
      `${file}'s detect handler rebuilds the menu without asking whether the human is mid-type in the custom box`
    );
  }
});
