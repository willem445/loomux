// A detection reply refreshes every surface it invalidates — on BOTH hosts, by
// BOTH routes (#993, #997 review blocking 1, reshaped by #1020).
//
// STRUCTURAL, and read off the source tree on purpose. This is DOM-wiring, and
// this repo validates DOM wiring by hand rather than by simulating a DOM
// (CLAUDE.md, code conventions) — so the thing that went wrong here cannot be
// reached by a behavioural test at all. What went wrong was a *missing call*:
// the workflow block editor repainted the model dropdown and nothing else,
// while its sibling reply handler four hundred lines up re-ran the analysis pass
// and repainted the knob rows. The data was fresh the whole time; the DOM was
// stale.
//
// The failure that produces is not cosmetic. `knobLookup` answers differently
// once a reply lands, so the Thinking-level row goes on offering `xhigh` for a
// model whose reply says it has no effort setting — the human picks it, the next
// mutation re-renders the row disabled, and the findings pane flags the block.
// The editor offered a value its own validator rejects, which is the exact thing
// `workflowview.ts`'s own comment claims cannot happen.
//
// **What #1020 changed about the subject, and what it did not.** The reply used
// to arrive from an `onDetect:` button hook; the button is gone, and a reply now
// arrives on two routes instead — the LOOKUP a picker fires when it paints, and
// the PUSH from the backend's startup sweep (`modelCatalog.onReport`). The
// refresh each one owes is identical, so each host funnels both into ONE
// method, and this file's job moved accordingly: it pins that the funnel exists,
// that it does the work, and that the routes reach it.
//
// It also pins something the button architecture did not need: the lookup fires
// from a RENDER path, and its own refresh ends in a re-render, so an unguarded
// handler is an infinite loop rather than a stale row. See the re-entry test.
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
//     `renderFindings()` on a path that never runs still passes.
//   - **NOTHING in this file is a behaviour test, including the
//     `runWhenNotEditing` pins at the bottom.** Those are source scans of a
//     second file, one level down. A behaviour pin genuinely is not available:
//     the method reads `document.activeElement` and attaches a listener, and
//     this repo forbids simulating a DOM (CLAUDE.md), so whether a deferral
//     actually defers is hand-validated like the rest of the DOM wiring. An
//     earlier version of this header said the opposite and it was never true
//     (#997 review B-1).
//   - It reads the SOURCE, not the module graph, so a refresher reached through
//     an indirection it cannot see reads as absent. That is the safe direction:
//     it under-recognises rather than over-claiming — but only because FULL-LINE
//     comments are stripped first. A trailing comment after code is NOT
//     stripped and could still satisfy an assertion; that is an accepted
//     residual (stripping those safely needs a tokenizer, not a regex).
//     Before this scan stripped anything, prose in a full-line comment satisfied
//     the assertions too, which made this exact sentence false outright
//     (#997 review B-2). See `stripComments`.
//   - The re-entry guard is checked in a WINDOW of source before the call, not
//     by parsing the enclosing `if`. A guard written far above its call, or
//     spelled through a helper, reads as absent. Safe direction again, and the
//     window is generous enough for a comment-free rewrite.
//
// It covers EVERY call site in each file, not just the first (#997 review
// NB-4). There is one per host today, so first-only would be sound as written —
// which is exactly why it is worth not doing: a second picker would go
// unchecked while the suite stayed green, and a scan that silently covers a
// subset is worse than no scan, because it reads as coverage.
//
// The vacuity guards below are load-bearing, not decoration: they fail if the
// region this thinks it is scanning stops being one. They caught two bugs in the
// scanner itself before this file was correct.
//
// **Every limit above was found by somebody mutating this file's subject and
// watching it stay green.** That is the only way a structural test's claims get
// checked, and it is the standard this file is held to precisely because it
// reads as broader coverage than it has.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const src = (file: string): string =>
  readFileSync(fileURLToPath(new URL(`../src/${file}`, import.meta.url)), "utf8");

/** Source with line comments removed, which is what every assertion here
 *  matches against (#997 review B-2).
 *
 *  **Without this the scan over-claims, and the header's stated safety direction
 *  is simply false.** These handlers are roughly two-thirds comment by volume,
 *  and those comments name every refresher in prose — so deleting the real
 *  `analyzeWorkflow(...)` / `renderFindings()` calls while leaving a comment
 *  that mentions them kept the whole suite green. That is the round-1 blocking
 *  regression passing the pin written for it.
 *
 *  The realistic path there is not a synthetic edit: it is somebody lifting
 *  these refreshers into a helper and leaving the explanatory comments in place.
 *  This file promises that an indirection it cannot see reads as **absent**; a
 *  comment that names the call made it read as present, which is the one
 *  direction a structural test must never fail in.
 *
 *  Full-line comments only, matching `test/workspacelayout.test.ts` (#991) —
 *  the repo already has this discipline and a second spelling of it would be a
 *  second thing to get right. A TRAILING comment after code is NOT stripped
 *  and could still satisfy an assertion below — prose carries the same
 *  parentheses a call does (`renderFindings()`, `repaintBlockKnobs?.()`, both
 *  used in this very file's own comments), so the call-shape assertions do not
 *  save it. That is an accepted residual: stripping a trailing comment safely
 *  needs a tokenizer (a bare regex would mangle a `"https://…"` string
 *  literal), and the full-line form is the repo's existing discipline. */
const stripComments = (text: string): string => text.replace(/^[ \t]*\/\/.*$/gm, "");

/** The source of the statement containing the `.detect(` call at `at`.
 *
 *  Runs to the `;` that ends the statement rather than to a balanced pair: the
 *  call is a chained expression (`detect(cli).then(...)`), whose `(cli)` returns
 *  the depth to zero long before the handler body is over. Throws rather than
 *  returning `""` if the statement never ends, so a shape this scan does not
 *  understand fails loudly instead of passing vacuously. */
function detectStatementAt(text: string, at: number, where: string): string {
  let depth = 0;
  for (let i = at; i < text.length; i += 1) {
    const c = text[i];
    if (c === "(" || c === "{" || c === "[") depth += 1;
    else if (c === ")" || c === "}" || c === "]") depth -= 1;
    else if (c === ";" && depth === 0) return text.slice(at, i);
  }
  throw new Error(`${where}: the \`.detect(\` call at ${at} has no statement end — this scan cannot read it`);
}

/** Every `<catalog>.detect(` call site in `text`, as (statement, offset) pairs.
 *
 *  Matches both spellings the two hosts use — `modelCatalog.detect(` in the
 *  workflow pane, `this.catalog.detect(` in the launcher — rather than a bare
 *  `.detect(`, which would also catch an unrelated method some future module
 *  happens to name that way.
 *
 *  Asserts at least one, so a renamed seam fails loudly instead of vacuously
 *  passing over an empty list — the same reason every assertion here has a
 *  vacuity guard. */
function detectCallSites(rawText: string, where: string): { body: string; at: number }[] {
  // Stripped BEFORE extraction, not after, so the statement scan is
  // comment-blind too: a commented-out call is not a call, and prose in a
  // comment can no longer satisfy an assertion downstream (#997 review B-2).
  const text = stripComments(rawText);
  const sites: { body: string; at: number }[] = [];
  const marker = /(?:modelCatalog|this\.catalog)\.detect\(/g;
  for (let m = marker.exec(text); m !== null; m = marker.exec(text)) {
    sites.push({ body: detectStatementAt(text, m.index, where), at: m.index });
  }
  assert.notEqual(
    sites.length,
    0,
    `${where} no longer calls \`.detect(\` — this scan is looking at the wrong thing`
  );
  return sites;
}

/** The top-level arguments of the `marker` call inside `text`, each trimmed.
 *
 *  Split at depth-zero commas rather than by regex, because the thing being
 *  asserted IS the arity: a pattern that merely correlates with two arguments is
 *  satisfied by any comma in a nested call, which is exactly the near-vacuous
 *  assertion this replaced (rev-721 NB-9).
 *
 *  Limits, stated rather than left to be discovered: it counts delimiters, not
 *  tokens, so a comma inside a string literal or a template placeholder would
 *  split an argument in two. Neither subscription contains one today, and the
 *  arity assertion that follows would fail LOUDLY rather than silently pass if
 *  one appeared — the safe direction, and the same trade the rest of this file
 *  makes. Throws if the call has no balanced argument list, so a shape this
 *  cannot read is never mistaken for a shape that passes. */
function callArguments(text: string, marker: string, where: string): string[] {
  const at = text.indexOf(marker);
  assert.notEqual(at, -1, `${where}: \`${marker}\` is not in the extracted region — this scan is reading the wrong thing`);
  const open = at + marker.length;
  const args: string[] = [];
  let depth = 0;
  let start = open;
  for (let i = open; i < text.length; i += 1) {
    const c = text[i];
    if (c === "(" || c === "{" || c === "[") depth += 1;
    else if (c === ")" || c === "}" || c === "]") {
      if (depth === 0) {
        args.push(text.slice(start, i).trim());
        return args.filter((a) => a !== "");
      }
      depth -= 1;
    } else if (c === "," && depth === 0) {
      args.push(text.slice(start, i).trim());
      start = i + 1;
    }
  }
  throw new Error(`${where}: the argument list after \`${marker}\` is unbalanced — this scan could not read it`);
}

/** Every `<catalog>.onReport(` subscription body in `text`. The push route's
 *  half of the same contract; same extraction rule, same vacuity guard. */
function onReportBodies(rawText: string, where: string): string[] {
  const text = stripComments(rawText);
  const bodies: string[] = [];
  const marker = /(?:modelCatalog|this\.catalog)\.onReport\(/g;
  for (let m = marker.exec(text); m !== null; m = marker.exec(text)) {
    bodies.push(detectStatementAt(text, m.index, where));
  }
  assert.notEqual(
    bodies.length,
    0,
    `${where} no longer subscribes to \`onReport\` — a form already open when the startup sweep answers ` +
      `would keep its curated seed for the life of the app, and nothing else would notice`
  );
  return bodies;
}

test("the workflow pane's detection refresh reaches the knobs, the findings and the menu", () => {
  const text = stripComments(src("workflowview.ts"));
  const at = text.indexOf("private applyDetection(");
  assert.notEqual(at, -1, "`applyDetection` was renamed — both routes' refresh has lost the thing they funnel into");
  const body = text.slice(at, text.indexOf("\n  }", at));
  assert.ok(body.length > 40, `the extracted method is implausibly short, so this test is not reading it: ${body}`);

  // The three the sibling `agent_cli_knobs` handler does, and the ones the first
  // cut of the old button handler did none of.
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
      "`renderInspector()` nulls it precisely so a late reply cannot paint into a row it has already detached"
  );
  assert.match(
    body,
    /refreshBlockModels\?\.\(\)/,
    "and the MENU through the live `this.refreshBlockModels?.()` — same argument, same null-clearing. Without it " +
      "a reply that lands while the human is inside the form repaints the knobs and leaves the dropdown stale"
  );
  // The NEGATIVE half, and it is the half that discriminates: naming the
  // form-local closure directly is what makes reverting either live hook redden.
  assert.doesNotMatch(
    body,
    /repaintKnobs\(\)/,
    "the refresh must not call the form-local `repaintKnobs()` closure at all — it walks around the null-clearing " +
      "that stops a late reply painting a detached row"
  );
});

test("each host's refresh funnel is idempotent per CLI, so the two routes cannot both apply", () => {
  // **rev-721 NB-8.** The push and the pull are two deliveries of ONE sweep
  // answer and both can land. The catalog-level test (*the two routes racing
  // does not repaint a form twice*) pins `acceptReport`'s refusal, which only
  // covers the PULL-FIRST ordering — the one that already worked. The
  // push-first ordering is guarded solely by these host-side marks, and rev-721
  // verified that deleting BOTH left the whole suite green: a fix whose removal
  // nothing notices is a fix that comes back out.
  //
  // Both halves are asserted because either one alone is inert. Without the
  // `has(...)` early-return the mark is written and never read; without the
  // `add(...)` the guard is read and never true. The `applyDetection` /
  // `refreshRoleFromDetection` scans above already prove these funnels are what
  // BOTH routes call, which is what makes a mark inside them sufficient.
  //
  // Reachability by name, like everything else in this file — see the header.
  const FUNNELS = [
    { file: "workflowview.ts", fn: "private applyDetection(" },
    { file: "launcher.ts", fn: "private refreshRoleFromDetection(" },
  ] as const;
  for (const { file, fn } of FUNNELS) {
    const text = stripComments(src(file));
    const at = text.indexOf(fn);
    assert.notEqual(at, -1, `${file}: \`${fn}\` was renamed — this scan is asserting a name that is gone`);
    const body = text.slice(at, text.indexOf("\n  }", at));
    assert.match(
      body,
      /if \(this\.detectionsApplied\.has\([^)]*\)\) return;/,
      `${file}'s funnel must refuse a delivery it has already applied — otherwise the push-first ordering repaints ` +
        `twice, and the second rebuild can land under a caret because a deferred one fires on blur`
    );
    assert.match(
      body,
      /this\.detectionsApplied\.add\(/,
      `${file}'s funnel must RECORD the delivery it just applied — a guard that is never made true is a guard that ` +
        `never fires, and the suite would stay green either way`
    );
  }
});

test("the workflow pane's detection refresh never rebuilds the form unconditionally", () => {
  // #997 review NB3-1. `replaceChildren` destroys the input under the caret, so
  // the pane's own rule — "the form is redrawn only when the human isn't inside
  // it" — has to hold here too. Round 4 of #997 shipped the fix but not a pin
  // for it: reverting the early-out to a bare rebuild left the whole suite
  // green. This closes that gap: every `renderInspector()` in the refresh must
  // be the `else` of the `contains(document.activeElement)` test, never bare.
  const text = stripComments(src("workflowview.ts"));
  const at = text.indexOf("private applyDetection(");
  const body = text.slice(at, text.indexOf("\n  }", at));
  assert.match(
    body,
    /contains\(document\.activeElement\)/,
    "the refresh has to ASK whether the human is inside the form before it decides how to repaint"
  );
  assert.doesNotMatch(
    body,
    /(?<!else )this\.renderInspector\(\)/,
    "a detection reply must never rebuild the form unconditionally: `replaceChildren` destroys the input under " +
      "the caret. The rebuild is always the `else` of the `contains(document.activeElement)` test"
  );
});

test("the launcher's detection refresh reaches the knob row and defers the menu", () => {
  const text = stripComments(src("launcher.ts"));
  const at = text.indexOf("private refreshRoleFromDetection(");
  assert.notEqual(at, -1, "`refreshRoleFromDetection` was renamed — both routes' refresh has lost its funnel");
  const body = text.slice(at, text.indexOf("\n  }", at));
  assert.ok(body.length > 40, `the extracted method is implausibly short, so this test is not reading it: ${body}`);

  assert.match(
    body,
    /applyRoleKnobs\(/,
    "a detection narrows which effort levels this role's model can take, so the knob row owes a repaint — " +
      "otherwise the form offers a level the payload then drops"
  );
  assert.match(
    body,
    /setOptions\(/,
    "…and the dropdown owes one too: the reply is where the CLI's real per-host ids come from"
  );
  // Not `applyRoleModels`, and this is the re-entry guard's launcher half:
  // `applyRoleModels` is what ISSUES the lookup, so calling it from the lookup's
  // own answer would re-enter it on every reply.
  assert.doesNotMatch(
    body,
    /this\.applyRoleModels\(/,
    "the refresh must not go through `applyRoleModels` — that method issues the lookup this is the answer to, " +
      "so reaching it here makes every reply re-enter the path that produced it"
  );
});

test("both hosts DEFER the mid-type menu rebuild rather than dropping it", () => {
  // #997 review NB-3 (round 2), then NB-3 again (round 3). An answer can land
  // while a human is mid-type in the `custom…` box, and rebuilding the menu
  // under a half-typed id hides the input beneath the caret. Round 2 got the
  // second half WRONG: it dropped the rebuild instead of postponing it, which on
  // the launcher was permanent — `applyRoleModels` is otherwise reachable only
  // from the CLI `change` listener and the seed pass, so the human saw detection
  // do nothing at all for the rest of the dialog's life.
  //
  // #1020 widens the window this guards rather than closing it: the reply now
  // arrives on the sweep's schedule instead of a click's, so the human is more
  // likely to be typing when it does, not less.
  //
  // The pin is on `runWhenNotEditing`, not on `editingCustom`: guarding is
  // necessary but not sufficient, and the bare predicate is what the dropped
  // version also used.
  const launcher = stripComments(src("launcher.ts"));
  const at = launcher.indexOf("private refreshRoleFromDetection(");
  const body = launcher.slice(at, launcher.indexOf("\n  }", at));
  assert.match(
    body,
    /runWhenNotEditing\(/,
    "the launcher's refresh must DEFER the menu rebuild past the mid-type window, not drop it — a guard that " +
      "only refuses is a detection that silently does nothing"
  );

  // The workflow pane routes its rebuild through the live hook, so the deferral
  // lives where the hook is INSTALLED — in the form that owns the picker. There
  // are two assignments to that field and they are different statements: the
  // null-clearing in `renderInspector`, which is what stops a late reply
  // painting a detached row, and the install. Both are required, so both are
  // read out rather than whichever `indexOf` happens to reach first.
  const wf = stripComments(src("workflowview.ts"));
  const assignments: string[] = [];
  const marker = /this\.refreshBlockModels = /g;
  for (let m = marker.exec(wf); m !== null; m = marker.exec(wf)) {
    assignments.push(wf.slice(m.index, wf.indexOf("\n", m.index)));
  }
  assert.notEqual(assignments.length, 0, "the pane no longer has a `refreshBlockModels` hook — its menu refresh is dead");
  assert.ok(
    assignments.some((a) => a.includes("= null")),
    `no assignment clears \`refreshBlockModels\` — without one, a reply landing after the human selected another ` +
      `block rebuilds the picker they left behind: ${assignments.join(" | ")}`
  );
  assert.ok(
    assignments.some((a) => /runWhenNotEditing\(/.test(a)),
    `the workflow pane's menu hook must defer past the mid-type window too — it is the same picker and the same ` +
      `hazard: ${assignments.join(" | ")}`
  );
});

test("a detection lookup fired from a render path cannot re-enter that render", () => {
  // **New with #1020, and the hazard the button architecture did not have.**
  // The lookup fires when a picker paints, and the refresh it triggers ends in
  // `renderInspector()` — which repaints the picker, which fires the lookup. The
  // memo makes the second call free but NOT a no-op: it resolves with the same
  // report, and an unguarded handler refreshes again, forever.
  //
  // Two independent exits, and both are required. The `models.length` early-out
  // stops a barren answer looping (it never sets `report()`, so the other guard
  // would not fire); the `report()` guard stops a good one (it would pass the
  // `models.length` test every time).
  for (const { body, at } of detectCallSites(src("workflowview.ts"), "workflowview.ts")) {
    assert.match(
      body,
      /\.models\.length/,
      "the handler must return early on an answer that carried nothing — it never sets `report()`, so it would " +
        "otherwise re-enter the render on every repaint"
    );
    const before = stripComments(src("workflowview.ts")).slice(Math.max(0, at - 400), at);
    assert.match(
      before,
      /!\s*modelCatalog\.report\(/,
      "the lookup must be guarded on there being no report in hand yet: its own refresh re-renders this form, " +
        "which re-runs this line, and a good answer passes the `models.length` test every time"
    );
  }
});

test("both hosts take the startup sweep's push, and answer for their own liveness", () => {
  // The route that matters most at boot: loomux opens showing the launcher, so
  // the common case is a form already on screen while the sweep is still
  // running. Its lookup was answered "nothing yet"; without this subscription
  // its dropdowns keep the curated seeds until the human closes and reopens.
  //
  // The liveness half is not decoration. Neither host has a teardown
  // `ModelCatalog` can rely on, so a subscription that never says it is gone
  // repaints detached DOM for the rest of the app run — one leak per discarded
  // form.
  // The liveness half is not decoration, and the shape it takes is the fix for
  // the leak the first cut shipped (rev-713 blocking 2). Liveness is a SECOND
  // argument — a side-effect-free predicate the catalog can ask at any time —
  // rather than the delivery callback's return value. When it was the return
  // value, asking meant delivering, so pruning could only happen when a report
  // changed state; the producer does that at most once per program per app run
  // and sometimes never, and every host built afterwards was retained forever.
  const LIVENESS = { "workflowview.ts": /!this\.disposed/, "launcher.ts": /aliveForReports\(\)/ };
  for (const file of ["workflowview.ts", "launcher.ts"] as const) {
    for (const body of onReportBodies(src(file), file)) {
      // rev-721 NB-9. This was `/onReport\(\s*[\s\S]*,[\s\S]*\)/`, which any
      // comma anywhere in the statement satisfies — including one inside an
      // argument list in the handler body — so it could not fail for the reason
      // its message gave. Split the argument list at depth instead: the shape
      // being asserted is genuinely arity, so count it rather than pattern-match
      // something that correlates with it.
      const args = callArguments(body, "onReport(", `${file}'s onReport`);
      assert.equal(
        args.length,
        2,
        `${file} must pass \`onReport\` a liveness predicate as its OWN argument, not fold liveness into the ` +
          `delivery callback: the catalog has to be able to ask "are you still there?" without repainting, which ` +
          `is the whole fix for the retention leak. Got ${args.length} argument(s): ${JSON.stringify(args)}`
      );
      assert.match(
        args[1],
        /^\(\s*\)\s*=>/,
        `${file}'s liveness argument must be a zero-argument predicate the catalog can call at any time — it is ` +
          `invoked when no repaint is wanted, so it must not be the delivery callback wearing a second hat: ${args[1]}`
      );
      assert.match(
        body,
        LIVENESS[file],
        `${file}'s subscription must hand the catalog its real liveness answer; without one the app-scoped ` +
          `catalog retains this host, its state and its detached DOM for the life of the process`
      );
    }
  }
  // …and each one reaches its host's funnel, which is what makes the push and
  // the lookup do the same work rather than merely similar work.
  assert.match(
    onReportBodies(src("workflowview.ts"), "workflowview.ts").join("\n"),
    /this\.applyDetection\(/,
    "the workflow pane's push must go through the same refresh the lookup does"
  );
  assert.match(
    onReportBodies(src("launcher.ts"), "launcher.ts").join("\n"),
    /this\.refreshRoleFromDetection\(/,
    "the launcher's push must go through the same refresh the lookup does"
  );
});

test("a host with a teardown releases its subscription there, rather than waiting to be pruned", () => {
  // The other half of blocking 2. `WorkflowView` HAS a teardown, so it must not
  // rely on the catalog's prune — that only runs when something else subscribes,
  // which may be never. The launcher deliberately has no equivalent: `fire()`
  // looks like one, but `reopenAfterLaunchFailure` revives the form afterwards,
  // so releasing there would leave a live form deaf to the sweep. That asymmetry
  // is why this names one file and not both, and it is argued at the launcher's
  // own subscription rather than left for a reader to infer from the absence.
  const wf = stripComments(src("workflowview.ts"));
  const at = wf.indexOf("dispose(): void {");
  assert.notEqual(at, -1, "`dispose` was renamed — the pane's release point is gone");
  const body = wf.slice(at, wf.indexOf("\n  }", at));
  assert.match(
    body,
    /unsubscribeReports\?\.\(\)/,
    "a pane with a real teardown must release its `onReport` subscription there: the catalog's prune is a backstop " +
      "for hosts that have no teardown, not a substitute for one that does"
  );
});

test("nothing in the picker can ask for a detection any more", () => {
  // #1020's deletion, pinned rather than assumed. Detection is automatic and the
  // backend sweep is the only thing that spawns an agent CLI; a `detect` button
  // would be an affordance for a spawn this control can no longer make, and its
  // presence is how the old architecture would grow back.
  const picker = stripComments(src("modelpicker.ts"));
  assert.doesNotMatch(picker, /onDetect/, "the `onDetect` hook is gone — the picker asks for nothing");
  assert.doesNotMatch(picker, /model-picker-detect/, "…and so is the button that carried it");
  assert.doesNotMatch(
    stripComments(src("styles.css")),
    /\.model-picker-detect/,
    "the button's CSS outlived the button — dead rules in the token layer read as a control somebody forgot to wire"
  );
});

test("the deferral schedules the rebuild rather than returning without it", () => {
  // **This is a source scan, one level down — NOT a behaviour test**, and saying
  // so is the whole point of #997 review B-1. `runWhenNotEditing` reads
  // `document.activeElement` and attaches a listener, and this repo forbids
  // simulating a DOM (CLAUDE.md), so whether a deferral actually defers is
  // hand-validated like the rest of the DOM wiring. An earlier version of this
  // comment claimed behaviour coverage; it never had any.
  const text = stripComments(src("modelpicker.ts"));
  // Anchored on the method DEFINITION, not the first occurrence of the name:
  // #2124 added a call site inside setOptions, and a first-occurrence anchor
  // read that call's surroundings as the deferral's body.
  const at = text.indexOf("runWhenNotEditing(rebuild");
  assert.notEqual(
    at,
    -1,
    "`runWhenNotEditing`'s definition moved or its parameter was renamed — the host scans above are asserting a name that is gone"
  );
  const body = text.slice(at, text.indexOf("\n  }", at));
  assert.match(body, /rebuild\(\)/, "the not-editing path must actually run the rebuild");
  assert.match(
    body,
    /addEventListener\("blur"[\s\S]*rebuild\(\)/,
    "and the editing path must schedule it for when the hazard ends, rather than returning without it"
  );
  assert.match(body, /once: true/, "one deferral, one rebuild — a listener left attached would fire on every later blur");
});

test("the deferral listens on the same element the edit guard reads", () => {
  // The mutation that exposed the false claim above (#997 review B-1): move the
  // listener from the custom input to the `<select>` and the suite stayed fully
  // green, while the user-visible NB-3 bug came straight back — `blur` does not
  // bubble, so the custom box's blur is then observed by nobody and the deferred
  // rebuild never runs at all.
  //
  // A behaviour pin for that is genuinely unavailable here (see the note above),
  // so this closes the hole the only way that is left: the guard and the
  // deferral are read out of the source SEPARATELY and required to name the same
  // field. It cannot prove the listener fires; it can prove the two halves have
  // not drifted apart, which is exactly the drift the mutation performed.
  const text = stripComments(src("modelpicker.ts"));

  const guardAt = text.indexOf("get editingCustom(");
  assert.notEqual(guardAt, -1, "`editingCustom` was renamed — this consistency check has lost one of its two halves");
  const guard = text.slice(guardAt, text.indexOf("\n  }", guardAt));
  const guarded = /this\.(\w+)\.hidden/.exec(guard);
  assert.notEqual(guarded, null, `the edit guard no longer reads a \`this.<field>.hidden\`, so this check cannot pair it: ${guard}`);

  // Same definition anchor as above: the first `runWhenNotEditing(` in the
  // file is now setOptions' own call site (#2124), not the deferral's body.
  const deferAt = text.indexOf("runWhenNotEditing(rebuild");
  const defer = text.slice(deferAt, text.indexOf("\n  }", deferAt));
  const listened = /this\.(\w+)\.addEventListener\("blur"/.exec(defer);
  assert.notEqual(listened, null, `the deferral no longer attaches a blur listener to a \`this.<field>\`: ${defer}`);

  assert.equal(
    listened?.[1],
    guarded?.[1],
    `the deferral waits on \`this.${listened?.[1]}\` but the guard watches \`this.${guarded?.[1]}\` — ` +
      `\`blur\` does not bubble, so the element the human is actually typing in would never release the deferred rebuild`
  );

  // …and the definition is not the only deferral any more: #2124 added one
  // inside setOptions, routed through the seam precisely so it cannot drift.
  // Sweep EVERY blur-attach site in the file, not just the first — the
  // mutation that produced this scan moved the listener to the `<select>` and
  // stayed green, and a second copy of the seam is where that drift grows
  // back. A site routed through `runWhenNotEditing` has no attach of its own
  // to check; a bypass that attaches directly is what this catches.
  const attaches = [...text.matchAll(/this\.(\w+)\.addEventListener\("blur"/g)].map((m) => m[1]);
  assert.notEqual(
    attaches.length,
    0,
    "no blur listener remains in modelpicker.ts — the deferral is gone and the host scans above assert a ghost"
  );
  for (const field of attaches) {
    assert.equal(
      field,
      guarded?.[1],
      `a blur listener on \`this.${field}\` but the edit guard watches \`this.${guarded?.[1]}\` — ` +
        `\`blur\` does not bubble, so a deferral waiting on any other element never releases`
    );
  }
});
