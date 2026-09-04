// The pure half of the left panel's tab host (#2122 slice B): which tab a
// toggle lands on, and what survives a restart. DOM-free so `node --test` can
// drive every transition; `src/leftpanel.ts` is the DOM that applies them and
// is hand-validated (CLAUDE.md's split).

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  DEFAULT_LEFT_PANEL_TAB,
  LEFT_PANEL_TABS,
  LEFT_PANEL_TAB_KEY,
  decodeLeftPanelTab,
  toggleTarget,
  type LeftPanelState,
} from "../src/leftpanelmodel.ts";

const state = (visible: boolean, tab: "sessions" | "agents"): LeftPanelState => ({ visible, tab });

test("a closed panel opens on whichever tab was asked for", () => {
  assert.deepEqual(toggleTarget(state(false, "sessions"), "agents"), state(true, "agents"));
  assert.deepEqual(toggleTarget(state(false, "agents"), "sessions"), state(true, "sessions"));
});

test("toggling the tab that is already showing closes the panel", () => {
  assert.deepEqual(toggleTarget(state(true, "agents"), "agents"), state(false, "agents"));
});

test("a closed panel remembers the tab it was closed on", () => {
  // The close keeps `tab` rather than resetting it, so the button that closed
  // the panel reopens the same view — and so the persisted tab is the one the
  // human last looked at, not the one they last dismissed.
  const closed = toggleTarget(state(true, "agents"), "agents");
  assert.equal(closed.tab, "agents");
  assert.deepEqual(toggleTarget(closed, "agents"), state(true, "agents"));
});

// THE CONSTRAINT-1 TEST. `#sessions` is an in-flow panel: its width change is
// the one discrete human click CLAUDE.md sanctions, and `resizeburst.ts`
// coalesces the fit that follows. A tab switch inside the panel must therefore
// move no column at all — `visible` is what the width is bound to, so a switch
// that flipped it would autosize every pane in the tab for a gesture that
// changes nothing about how much room the terminals get.
test("switching tabs on an open panel never changes visibility", () => {
  for (const from of LEFT_PANEL_TABS) {
    for (const to of LEFT_PANEL_TABS) {
      if (from === to) continue;
      const next = toggleTarget(state(true, from), to);
      assert.equal(next.visible, true, `${from} -> ${to} closed the panel`);
      assert.equal(next.tab, to);
    }
  }
});

test("the persisted tab decodes totally, defaulting on anything unusable", () => {
  assert.equal(decodeLeftPanelTab(null), DEFAULT_LEFT_PANEL_TAB);
  assert.equal(decodeLeftPanelTab(""), DEFAULT_LEFT_PANEL_TAB);
  assert.equal(decodeLeftPanelTab("dock"), DEFAULT_LEFT_PANEL_TAB);
  assert.equal(decodeLeftPanelTab("SESSIONS"), DEFAULT_LEFT_PANEL_TAB);
  for (const tab of LEFT_PANEL_TABS) assert.equal(decodeLeftPanelTab(tab), tab);
});

test("the preference key follows the loomux.* UI-chrome convention", () => {
  assert.match(LEFT_PANEL_TAB_KEY, /^loomux\./);
});
