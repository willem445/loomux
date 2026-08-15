import { test } from "node:test";
import assert from "node:assert/strict";

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
  encodeDockPrefs,
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
  assert.deepEqual(
    decideFollow({ open: true, dockRoot: "C:\\a", paneCwd: "C:\\b" }),
    { kind: "adopt", root: "C:\\b" }
  );
});

test("a CLOSED dock parks the root and builds nothing", () => {
  // The "no work while closed" requirement. `park` must NOT be `adopt`: adopting
  // is what constructs and refreshes views, and a closed dock has no business
  // doing either while the human is looking at terminals.
  assert.deepEqual(
    decideFollow({ open: false, dockRoot: null, paneCwd: "C:\\b" }),
    { kind: "park", root: "C:\\b" }
  );
});

test("a parked root is redeemed by re-asking with the dock open", () => {
  // There is no second entry point for the parked value — opening the dock runs
  // the same decision with the parked root as the cwd, which must now adopt.
  const parked = decideFollow({ open: false, dockRoot: null, paneCwd: "C:\\b" });
  assert.equal(parked.kind, "park");
  const root = parked.kind === "park" ? parked.root : "";
  assert.deepEqual(decideFollow({ open: true, dockRoot: null, paneCwd: root }), {
    kind: "adopt",
    root: "C:\\b",
  });
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

test("a same-root focus change is inert even while the dock is closed", () => {
  // "none" beats "park": there is nothing to remember, so nothing should be
  // recorded that would make the next open look like a pending re-root.
  assert.deepEqual(decideFollow({ open: false, dockRoot: "C:\\a", paneCwd: "C:\\a" }), { kind: "none" });
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

test("the dock may never cover the whole workspace", () => {
  // The dock is an overlay: nothing about the grid pushes back on its width, so
  // without this a wide dock hides every pane while the panes carry on
  // rendering full-size underneath it.
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

test("a persisted width is clamped on the way in as well as out", () => {
  // A pref written by a wider monitor, or hand-edited, must not restore a dock
  // that covers the app.
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
