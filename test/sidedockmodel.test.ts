import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  DEFAULT_DOCK_PREFS,
  DOCK_MAX_W,
  DOCK_MIN_W,
  DOCK_TABS,
  DOCK_TERM_RESERVE_PX,
  clampDockWidth,
  decideFollow,
  decideViewSync,
  decodeDockPrefs,
  dockBoxes,
  encodeDockPrefs,
  followsPaneChange,
  isActiveTabChange,
  isDockTab,
  normalizeDockRoot,
  type DockPrefs,
} from "../src/sidedockmodel.ts";

// ---------- normalizeDockRoot ----------

test("a trailing separator is not a different folder", () => {
  // The two sources the dock reads disagree about this for the SAME directory:
  // a pane's launch cwd carries no trailing slash, a shell's OSC 7 report can.
  // If they compared unequal, every focus change would rebuild every view.
  assert.equal(normalizeDockRoot("C:\\Projects\\loomux\\"), normalizeDockRoot("C:\\Projects\\loomux"));
  assert.equal(normalizeDockRoot("/home/w/loomux/"), normalizeDockRoot("/home/w/loomux"));
  assert.equal(normalizeDockRoot("C:\\Projects\\loomux\\\\"), "C:\\Projects\\loomux");
});

test("a filesystem root survives normalization as a root", () => {
  // Stripping every trailing separator would reduce these to a drive letter or
  // to nothing — a path that no longer names the folder it came from.
  assert.equal(normalizeDockRoot("C:\\"), "C:\\");
  assert.equal(normalizeDockRoot("C:/"), "C:\\");
  assert.equal(normalizeDockRoot("/"), "/");
});

test("an absent or blank cwd is no root at all", () => {
  assert.equal(normalizeDockRoot(null), null);
  assert.equal(normalizeDockRoot(undefined), null);
  assert.equal(normalizeDockRoot(""), null);
  assert.equal(normalizeDockRoot("   "), null);
});

// ---------- decideFollow ----------

test("focusing a pane in another repo re-roots an open dock", () => {
  assert.deepEqual(decideFollow({ open: true, dockRoot: "C:\\a", paneCwd: "C:\\b" }), {
    kind: "adopt",
    root: "C:\\b",
  });
});

test("a CLOSED dock does nothing at all — it does not even record a root", () => {
  // The "no work while closed" requirement. It must not be `adopt` (which is
  // what constructs and rebuilds views), and it must not be a third
  // "remember this for later" state either: the dock keeps no pending root, so
  // opening it re-reads the LIVE cwd instead of replaying a stale one.
  assert.deepEqual(decideFollow({ open: false, dockRoot: null, paneCwd: "C:\\b" }), { kind: "none" });
  assert.deepEqual(decideFollow({ open: false, dockRoot: "C:\\a", paneCwd: "C:\\b" }), { kind: "none" });
});

test("opening the dock adopts the live cwd — the real redemption path", () => {
  // Exactly the pair `show()` runs: `open` has just been set true, the dock has
  // never adopted a root, and the cwd is pulled fresh. An earlier version
  // recorded the root on the CLOSED call, which made this call a no-op and made
  // the `adopt` its own test witnessed unreachable from the real flow (N3).
  assert.deepEqual(decideFollow({ open: true, dockRoot: null, paneCwd: "C:\\b" }), {
    kind: "adopt",
    root: "C:\\b",
  });
  // Re-opening on the folder it was already showing is inert.
  assert.deepEqual(decideFollow({ open: true, dockRoot: "C:\\b", paneCwd: "C:\\b" }), { kind: "none" });
});

test("focusing an SSH or welcome pane does not blank the dock", () => {
  // An SSH pane's OSC 7 names a path on the FAR end, so Pane reports no local
  // cwd for it at all; a welcome pane has none yet. Neither is a request to
  // empty the sidebar.
  assert.deepEqual(decideFollow({ open: true, dockRoot: "C:\\a", paneCwd: null }), { kind: "none" });
  assert.deepEqual(decideFollow({ open: true, dockRoot: "C:\\a", paneCwd: "  " }), { kind: "none" });
});

test("re-focusing a pane in the folder already shown is not a re-root", () => {
  assert.deepEqual(decideFollow({ open: true, dockRoot: "C:\\a", paneCwd: "C:\\a" }), { kind: "none" });
  // ...including when only a trailing separator differs, which is the case that
  // actually reaches this code — see normalizeDockRoot above.
  assert.deepEqual(decideFollow({ open: true, dockRoot: "C:\\a", paneCwd: "C:\\a\\" }), { kind: "none" });
});

// ---------- isActiveTabChange: which notifications may move the dock ----------

test("a tab notification that leaves the active tab alone must NOT move the dock", () => {
  // THE REGRESSION PIN for the follow trigger. TabManager.onChange is a
  // tab-SET listener: it also fires on rename, colour, reorder, and — the one
  // that actually bit — setTabAttention, every time a background agent's
  // attention flips. Following it unfiltered means the dock re-reads the active
  // pane's LIVE cwd at a moment the human did not cause, adopting a directory
  // change that was correctly ignored when it was typed, and rebuilding the
  // file explorer out from under them.
  assert.equal(isActiveTabChange("ws-1", "ws-1"), false);
  assert.equal(isActiveTabChange(null, null), false);
});

test("a pane change in a BACKGROUND tab must not move the dock", () => {
  // THE rev-776 REGRESSION PIN — the second door onto the same defect. Every
  // workspace has a grid, so every workspace gets an `onActive` callback; only
  // the foreground one may drive a follow. A background tab opening or closing
  // a pane (an agent finishing, a delegate spawning, a group resuming) calls
  // `setActive` on the survivor, and an ungated follow would then re-read the
  // FOREGROUND pane's live cwd and adopt a directory change the human made
  // earlier and had every reason to think was ignored.
  //
  // Reading the RIGHT pane is not the same as reading it at the right MOMENT,
  // which is what the first fix got wrong.
  assert.equal(followsPaneChange("ws-2", "ws-1"), false);
  assert.equal(followsPaneChange("ws-3", "ws-1"), false);
});

test("a pane change in the FOREGROUND tab does move the dock", () => {
  assert.equal(followsPaneChange("ws-1", "ws-1"), true);
});

test("before any tab exists, no pane change is in the foreground", () => {
  // Pins the CONTRACT at boot, not a guard: this falls out of the comparison
  // itself, because no real workspace id is null. Stated because mutating an
  // explicit null-check away reddened nothing — so the check was removed rather
  // than left standing as a guard that guards nothing.
  assert.equal(followsPaneChange("ws-1", null), false);
});

test("a genuine tab switch DOES move the dock", () => {
  // The trigger still has to exist: switching project tabs changes the active
  // pane without any grid's setActive firing, so without this the dock keeps
  // showing the previous tab's repo.
  assert.equal(isActiveTabChange("ws-1", "ws-2"), true);
  // Boot (no tab yet, then the first one) counts, and so does losing the last.
  assert.equal(isActiveTabChange(null, "ws-1"), true);
  assert.equal(isActiveTabChange("ws-2", null), true);
});

// ---------- decideViewSync ----------

test("a tab's view is constructed on first use, at the dock's root", () => {
  assert.equal(decideViewSync({ dockRoot: "C:\\a", builtRoot: null, dirty: false }), "build");
});

test("a clean view follows the dock to a new root", () => {
  assert.equal(decideViewSync({ dockRoot: "C:\\b", builtRoot: "C:\\a", dirty: false }), "rebuild");
});

test("a view already at the dock's root does nothing", () => {
  assert.equal(decideViewSync({ dockRoot: "C:\\a", builtRoot: "C:\\a", dirty: false }), "none");
  assert.equal(decideViewSync({ dockRoot: "C:\\a\\", builtRoot: "C:\\a", dirty: false }), "none");
});

test("an editor holding unsaved edits refuses to follow, and is not rebuilt", () => {
  // Rebuilding disposes the view, which throws its buffer away. Doing that
  // because the human clicked a different PANE would destroy work they never
  // agreed to lose (#219). It must be "hold" — anything else, "rebuild"
  // included, is the data-loss bug.
  assert.equal(decideViewSync({ dockRoot: "C:\\b", builtRoot: "C:\\a", dirty: true }), "hold");
});

test("a held editor resumes following the moment it is clean again", () => {
  // Saving or discarding clears `dirty`; the dock re-asks on every tab
  // activation, so nothing else has to notice that the buffer settled.
  assert.equal(decideViewSync({ dockRoot: "C:\\b", builtRoot: "C:\\a", dirty: true }), "hold");
  assert.equal(decideViewSync({ dockRoot: "C:\\b", builtRoot: "C:\\a", dirty: false }), "rebuild");
});

test("a dirty editor already at the dock's root is not 'held' — it is simply correct", () => {
  // "hold" is a stale-and-protected signal the UI shows a notice for. A dirty
  // buffer in the folder the dock is pointed at is neither stale nor a problem.
  assert.equal(decideViewSync({ dockRoot: "C:\\a", builtRoot: "C:\\a", dirty: true }), "none");
});

test("no root yet leaves an existing view standing rather than tearing it down", () => {
  assert.equal(decideViewSync({ dockRoot: null, builtRoot: null, dirty: false }), "none");
  assert.equal(decideViewSync({ dockRoot: null, builtRoot: "C:\\a", dirty: false }), "none");
});

// ---------- clampDockWidth ----------

test("a dock width is clamped to something readable", () => {
  assert.equal(clampDockWidth(10), DOCK_MIN_W);
  assert.equal(clampDockWidth(99999), DOCK_MAX_W);
  assert.equal(clampDockWidth(500), 500);
});

test("the dock may never take the whole workspace", () => {
  // Nothing about the grid pushes back on a persisted or dragged width, so
  // without this a wide dock would squeeze the panes out of the row entirely
  // (and, before #1150 made it displace, would have covered them instead —
  // same number, same promise, different mechanism).
  const workspace = 800;
  assert.equal(clampDockWidth(9999, workspace), workspace - DOCK_TERM_RESERVE_PX);
  assert.ok(clampDockWidth(9999, workspace) < workspace);
});

test("a workspace narrower than the reserve still yields a usable dock", () => {
  // Arithmetic, not a promise: on a window this small the reserve cannot be
  // honoured, and the minimum wins — the same degradation overlaysize.ts
  // documents for the height clamp.
  assert.equal(clampDockWidth(400, 300), DOCK_MIN_W);
});

test("a non-finite width falls back rather than reaching a style property", () => {
  // `NaN` in `style.width` collapses the dock to nothing, silently.
  assert.equal(clampDockWidth(Number.NaN), DEFAULT_DOCK_PREFS.width);
  assert.equal(clampDockWidth(Number.POSITIVE_INFINITY), DEFAULT_DOCK_PREFS.width);
});

// ---------- dockBoxes: the column, and the panel the column clips ----------

test("a closed dock's COLUMN is zero — that is how the panes get the space back (#1150)", () => {
  // The feature, in one assertion. The dock is a flex sibling of #grid-area, so
  // an open pane reclaims the room only if the dock's own column reaches 0; a
  // closed dock that kept its width would sit there as an empty strip and the
  // autosize the human asked for would never happen.
  assert.equal(dockBoxes(false, 420).columnPx, 0);
  assert.equal(dockBoxes(true, 420).columnPx, 420);
});

test("a closed dock's CONTENTS keep their width, so the toggle slides rather than reflows", () => {
  // Not an oversight — the inner panel is absolutely positioned at a fixed
  // width and the column clips it. If it collapsed with the column, every frame
  // of the 240 ms transition would re-lay-out a git graph or a file tree at an
  // intermediate width, and the reopen would do it again on the way back.
  assert.equal(dockBoxes(false, 420).contentPx, 420);
  assert.equal(dockBoxes(false, 420).contentPx, dockBoxes(true, 420).contentPx);
});

test("both widths come out of ONE call, so they cannot disagree", () => {
  // An open dock's column and its contents are the same number BY
  // CONSTRUCTION. Two separate clamps could round differently, or drift when
  // one caller was updated and the other was not, and the symptom would be a
  // 1px sliver of grid showing through the panel.
  for (const w of [DOCK_MIN_W - 50, 301, 420, 899, DOCK_MAX_W + 50]) {
    const boxes = dockBoxes(true, w);
    assert.equal(boxes.columnPx, boxes.contentPx, `width ${w}`);
    assert.equal(boxes.columnPx, clampDockWidth(w), `width ${w}`);
  }
});

test("a corrupt persisted width never reaches a style property", () => {
  // `style.width = "NaNpx"` is ignored, which leaves the column at whatever it
  // was — including, for a dock being closed, its full open width.
  for (const bad of [Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.equal(dockBoxes(true, bad).columnPx, DEFAULT_DOCK_PREFS.width);
    assert.equal(dockBoxes(false, bad).columnPx, 0);
    assert.equal(dockBoxes(false, bad).contentPx, DEFAULT_DOCK_PREFS.width);
  }
});

test("sidedock.ts hides the dock by COLLAPSING it, never with `hidden` (#1150)", () => {
  // The pure module being green says nothing about whether the DOM half uses
  // it. DOM wiring is hand-verified in this repo, but this particular wire has
  // a silent failure mode worth a guard: `el.hidden` is `display: none`, which
  // hides the dock perfectly well and looks like a tidy-up. It would also skip
  // the width transition entirely, so the panes would snap instead of
  // autosizing and the burst `resizeburst.ts` is sized for would never exist —
  // and every other test in this file would still pass.
  //
  // The axis is the DOM PROPERTY, not any identifier around it (a rename cannot
  // step over `.hidden = `), and it is default-deny in both directions: the one
  // `dockBoxes` call must be found, so moving the geometry somewhere else fails
  // here loudly rather than leaving this guard watching nothing.
  const src = readFileSync(new URL("../src/sidedock.ts", import.meta.url), "utf8");
  assert.doesNotMatch(
    src,
    /\bthis\.el\.hidden\s*=/,
    "the dock's own column must never be display:none — that is the transition, and the autosize"
  );
  assert.equal(
    [...src.matchAll(/\bdockBoxes\s*\(/g)].length,
    1,
    "sidedock.ts must set its geometry from exactly one dockBoxes call — a second one is a " +
      "second copy of the rule, and none means this guard is watching nothing"
  );
});

// ---------- the reserve's REAL enforcement point: the stylesheet ----------

/** One CSS rule body, read off the stylesheet by its selector.
 *
 *  Anchored to the start of a line and sliced to the rule's OWN closing brace,
 *  never `css.slice(css.indexOf(".sidedock {"), css.indexOf(".sidedock-grip"))`,
 *  which is what these two tests used to do. `indexOf` takes the FIRST
 *  occurrence of each end, so an earlier mention of either name — and `.sidedock`
 *  already appears in comments ~35 lines above the real rule — selects a region
 *  that is not the rule.
 *
 *  An EMPTY slice is not the hazard; that case is loud, because `assert.match`
 *  and `assert.ok(...includes...)` both throw on `""` (checked, not assumed).
 *  The hazard is a non-empty WRONG slice, and the specific way it goes wrong
 *  here is nasty: everything between the rule and the next name is swallowed,
 *  including OTHER rules whose declarations answer the assertion's question
 *  differently. Measured on today's stylesheet rather than argued: the naive
 *  `slice(indexOf(".sidedock {"), indexOf(".sidedock-grip"))` is 2023 chars and
 *  matches `position: absolute` — not from `.sidedock`, which is in flow, but
 *  from `.sidedock-inner`, which is legitimately absolute inside it. The
 *  in-flow assertion below would fail on a perfectly correct stylesheet; with
 *  the polarity the old overlay test had, the same slice would have PASSED on a
 *  broken one. Anchoring to a line-start selector and stopping at the rule's
 *  own brace is what makes both directions read the rule itself. */
function cssRule(selector: string): string {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const open = new RegExp(`^${escaped}\\s*\\{`, "m").exec(css);
  assert.ok(open, `the ${selector} rule must exist in src/styles.css`);
  const end = css.indexOf("}", open.index);
  assert.ok(end > open.index, `${selector}'s rule is unterminated`);
  const rule = css.slice(open.index, end + 1);
  assert.ok(
    /[a-z-]+\s*:/.test(rule),
    `${selector}'s rule body carries no declarations — the slice is wrong, not the CSS:\n${rule}`
  );
  return rule;
}

const sidedockRule = () => cssRule(".sidedock");

test("the stylesheet bounds the dock's width, so boot/restore/resize are all covered", () => {
  // `clampDockWidth`'s reserve only ever runs on the DRAG path. Boot, a restore
  // from persistence, and a window resize do not call it — so a width persisted
  // on a wide monitor used to come back whole on a narrow one and occlude every
  // pane. The bound that actually holds is CSS, and this reads it off disk.
  const rule = sidedockRule();
  const maxWidth = /max-width:\s*max\(\s*(\d+)px\s*,\s*calc\(\s*100%\s*-\s*(\d+)px\s*\)\s*\)/.exec(rule);
  assert.ok(maxWidth, `.sidedock needs a max-width bound; rule was:\n${rule}`);
  // PINNED both ways: the stylesheet cannot be a fourth copy of these numbers
  // that drifts from the module the tests reason about.
  assert.equal(Number(maxWidth[1]), DOCK_MIN_W, "the CSS floor must mirror DOCK_MIN_W");
  assert.equal(
    Number(maxWidth[2]),
    DOCK_TERM_RESERVE_PX,
    "the CSS reserve must mirror DOCK_TERM_RESERVE_PX"
  );
});

test("the dock is an IN-FLOW flex item, and the stylesheet is where that is true (#1150)", () => {
  // The dock used to be `position: absolute` — out of flow, occluding panes,
  // unable to squeeze `#grid-area` by construction. #1150 moved it into the
  // flex row at the human's direction so the open panes autosize the way
  // `#sessions` already makes them (doc/design/side-dock.md).
  //
  // Out of flow is therefore now the REGRESSION, not the invariant: an
  // absolutely-positioned `.sidedock` would silently stop displacing the grid,
  // the panes would go back to being covered, and every other test here would
  // still pass. Both spellings are refused, because `fixed` occludes the same
  // way `absolute` does.
  const rule = sidedockRule();
  assert.doesNotMatch(
    rule,
    /position:\s*(absolute|fixed)/,
    ".sidedock must stay in flow — out of flow is what #1150 removed"
  );
});

test("the dock's column animates, so its resize burst reaches the coalescer (#1150)", () => {
  // The whole point of animating rather than snapping: an animated width is a
  // BURST of ResizeObserver deliveries, which `src/resizeburst.ts` collapses
  // into one fit per pane per toggle. It is the same treatment `#sessions`
  // gets, through the same seam, with no second coalescer anywhere near the
  // dock. `test/resizeburst.test.ts` pins the duration against the ceiling.
  assert.match(
    sidedockRule(),
    /transition:[^;]*\bwidth\b/,
    ".sidedock must animate its width — that is the burst the coalescer is for"
  );
  // The fixed-width inner is what keeps the hosted git graph / file tree from
  // re-laying out at every intermediate width; without the clip it would paint
  // straight over the grid while the column collapses.
  assert.match(sidedockRule(), /overflow:\s*hidden/, ".sidedock must clip its fixed-width inner");
});

test("a closed dock collapses its width — it does not `display: none` (#1150)", () => {
  // `display: none` is the obvious way to hide a panel and it would break the
  // feature twice over: no transition can run from it, so the panes would snap
  // rather than autosize, and the burst the coalescer is sized for would never
  // exist. The closed state is a zero-width column (`dockBoxes`) plus this
  // rule, which only removes the border that would otherwise be a stray 1px
  // line down the grid's right edge.
  const rule = cssRule(".sidedock.collapsed");
  assert.doesNotMatch(rule, /display:/, "a collapsed dock is zero-width, never display:none");
  assert.match(rule, /border-left-width:\s*0/);
});

test("a drag suppresses the transition, or the dock lags the cursor (#1150)", () => {
  // The transition exists for the toggle. On the grip drag the width follows
  // the mouse, and a 240 ms ease on every mousemove makes the dock trail the
  // pointer AND turns a settled drag into a burst that keeps arriving after the
  // human stopped moving — the coalescer would then fit at a width the drag had
  // already left.
  assert.match(cssRule(".sidedock.resizing"), /transition:\s*none/);
});

test("the grid keeps its reserve however the two side panels are combined (#1150)", () => {
  // `.sidedock`'s `max-width` bounds what the dock ASKS for, against the whole
  // workspace. That was the entire guarantee while the dock only covered the
  // grid — but a dock that DISPLACES shares the row with `#sessions` (344px,
  // `flex: none`), and `max-width: calc(100% - 240px)` does not know that. With
  // both panels open on a 640px window the two fixed columns want 764px, and
  // the grid — `min-width: 0` before #1150 — would be squeezed to nothing while
  // both panels kept their full width.
  //
  // The floor therefore lives on the thing being protected: `#grid-area` keeps
  // DOCK_TERM_RESERVE_PX no matter what asks for room, and the dock (the only
  // shrinkable item in the row) gives the space up instead.
  const rule = cssRule("#grid-area");
  const min = /min-width:\s*(\d+)px/.exec(rule);
  assert.ok(min, `#grid-area needs a min-width floor; rule was:\n${rule}`);
  assert.equal(
    Number(min[1]),
    DOCK_TERM_RESERVE_PX,
    "the grid's floor must mirror DOCK_TERM_RESERVE_PX — it IS the reserve, now that the dock displaces"
  );
  // A floor the dock cannot push through is only half of it: the dock has to be
  // the item that yields. `flex: none` (what `#sessions` has) would overflow
  // the workspace instead of shrinking.
  assert.match(
    sidedockRule(),
    /flex:\s*0\s+1\s+auto/,
    ".sidedock must be shrinkable, so a squeezed row costs the dock and not the grid"
  );
});

// ---------- prefs ----------

test("a first run gets the defaults, and the dock starts closed", () => {
  // Closed by default because the dock OCCLUDES panes rather than shrinking
  // them; covering someone's terminal before they asked is not a default.
  assert.deepEqual(decodeDockPrefs(null), DEFAULT_DOCK_PREFS);
  assert.equal(DEFAULT_DOCK_PREFS.open, false);
});

test("prefs round-trip", () => {
  const p: DockPrefs = { open: true, tab: "editor", width: 512 };
  assert.deepEqual(decodeDockPrefs(encodeDockPrefs(p)), p);
});

test("garbage in localStorage does not throw and does not lose the dock", () => {
  for (const raw of ["", "not json", "[]", "null", "42", '"git"']) {
    assert.deepEqual(decodeDockPrefs(raw), DEFAULT_DOCK_PREFS, `raw: ${raw}`);
  }
});

test("one bad field costs only that field", () => {
  // Record-wise rejection would silently discard a whole preference on the next
  // boot after a stray hand-edit — the leniency tabstore.decodePane already applies.
  assert.deepEqual(decodeDockPrefs('{"open":true,"tab":"tasks","width":512}'), {
    open: true,
    tab: DEFAULT_DOCK_PREFS.tab,
    width: 512,
  });
  assert.deepEqual(decodeDockPrefs('{"open":"yes","tab":"files","width":512}'), {
    open: DEFAULT_DOCK_PREFS.open,
    tab: "files",
    width: 512,
  });
  assert.deepEqual(decodeDockPrefs('{"open":true,"tab":"files","width":"wide"}'), {
    open: true,
    tab: "files",
    width: DEFAULT_DOCK_PREFS.width,
  });
});

test("a persisted width is bounded to the absolute range on the way in", () => {
  // This path has NO live window width, so it can only apply DOCK_MIN_W /
  // DOCK_MAX_W — the workspace reserve is the stylesheet's job (see the
  // max-width test above). Stating that here rather than claiming
  // restore-safety this assertion does not actually witness.
  assert.equal(decodeDockPrefs('{"width":99999}').width, DOCK_MAX_W);
  assert.equal(decodeDockPrefs('{"width":-5}').width, DOCK_MIN_W);
  assert.equal(JSON.parse(encodeDockPrefs({ open: true, tab: "git", width: 99999 })).width, DOCK_MAX_W);
});

test("the tab set is exactly the three views the dock hosts", () => {
  // #934 also envisioned a tasks tab; it is deliberately out of scope here, and
  // this is the line that would notice one arriving without the wiring.
  assert.deepEqual([...DOCK_TABS], ["git", "files", "editor"]);
  assert.ok(DOCK_TABS.every(isDockTab));
  assert.equal(isDockTab("tasks"), false);
  assert.equal(isDockTab(undefined), false);
});
