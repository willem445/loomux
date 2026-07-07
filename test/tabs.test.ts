// TabManager unit tests (#63, project tabs): add/remove/switch, the active-tab
// invariant, "never zero tabs," the no-resize switch mechanism (hidden tabs are
// set display:none, i.e. setVisible(false) → zero-width panes → no PTY resize,
// see panefit.test.ts), and the phase-3 routing seams. Pure logic, DOM-free —
// driven with a fake workspace (CLAUDE.md test convention). Run `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { TabManager, type ManagedWorkspace } from "../src/tabs.ts";

/** A lightweight ManagedWorkspace that records the visibility/focus/dispose
 *  calls TabManager makes, so tests can assert the switch mechanism. */
class FakeWorkspace implements ManagedWorkspace {
  name = "";
  color: string | null = null;
  visible = false;
  focuses = 0;
  disposed = false;
  visLog: boolean[] = [];
  readonly id: string;
  constructor(id: string) {
    this.id = id;
  }
  setVisible(v: boolean): void {
    this.visible = v;
    this.visLog.push(v);
  }
  focus(): void {
    this.focuses++;
  }
  dispose(): void {
    this.disposed = true;
  }
}

function makeManager() {
  const created: FakeWorkspace[] = [];
  const tabs = new TabManager<FakeWorkspace>((id) => {
    const ws = new FakeWorkspace(id);
    created.push(ws);
    return ws;
  });
  return { tabs, created };
}

/** The single visible workspace, asserting exactly one is shown (the invariant
 *  that makes tab switching a zero-width, no-resize operation for the rest). */
function onlyVisible(tabs: TabManager<FakeWorkspace>): FakeWorkspace {
  const shown = tabs.tabs.filter((w) => w.visible);
  assert.equal(shown.length, 1, "exactly one tab is visible at a time");
  return shown[0];
}

test("the first tab becomes active and visible", () => {
  const { tabs } = makeManager();
  const ws = tabs.newTab();
  assert.equal(tabs.activeTabId, ws.id);
  assert.equal(tabs.count, 1);
  assert.equal(onlyVisible(tabs).id, ws.id);
});

test("adding tabs: a new active tab hides the previous one", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  const b = tabs.newTab();
  assert.equal(tabs.activeTabId, b.id);
  assert.equal(a.visible, false, "the previously-active tab is hidden");
  assert.equal(b.visible, true);
  assert.equal(onlyVisible(tabs).id, b.id);
});

test("newTab(activate=false) opens hidden and does not steal active", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  const b = tabs.newTab(false);
  assert.equal(tabs.activeTabId, a.id, "active is unchanged");
  assert.equal(b.visible, false, "the background tab is hidden");
  assert.equal(onlyVisible(tabs).id, a.id);
});

test("switching hides the old tab and shows+focuses the new one (no dispose)", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  const b = tabs.newTab(false);
  a.focuses = 0;
  b.focuses = 0;
  tabs.switchTo(b.id);
  assert.equal(a.visible, false, "the tab switched away from is hidden (zero width, no resize)");
  assert.equal(b.visible, true);
  assert.equal(b.focuses, 1, "the newly active tab's pane is focused");
  assert.equal(a.disposed, false, "switching never disposes — panes/scrollback survive");
  assert.equal(b.disposed, false);
});

test("switching to the already-active tab just refocuses it", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  a.focuses = 0;
  tabs.switchTo(a.id);
  assert.equal(a.focuses, 1);
  assert.equal(onlyVisible(tabs).id, a.id);
});

test("no-resize regression: a hidden tab is only ever set invisible, never re-shown while inactive", () => {
  // The mechanism guarantee: once a tab is switched away from, TabManager sets it
  // display:none and does NOT toggle it visible again until it is re-activated.
  // A stray setVisible(true) on an inactive tab would give its panes non-zero
  // width and re-arm applyFit → a PTY resize storm (the exact thing #63 avoids).
  const { tabs } = makeManager();
  const a = tabs.newTab();
  const b = tabs.newTab(); // activating b hides a
  const c = tabs.newTab(); // activating c hides b (a stays hidden)
  // Each non-active tab is shown once (at creation) and thereafter only ever set
  // invisible — never re-shown while inactive. A stray setVisible(true) would
  // give its panes non-zero width and re-arm applyFit (a resize storm).
  const shownOnceThenHidden = (ws: FakeWorkspace) => {
    assert.equal(ws.visLog[0], true, "shown once at creation");
    assert.equal(
      ws.visLog.slice(1).every((v) => v === false),
      true,
      "never re-shown while inactive"
    );
    assert.equal(ws.visible, false, "currently hidden");
  };
  shownOnceThenHidden(a);
  shownOnceThenHidden(b);
  assert.equal(c.visible, true, "c: the active tab, still shown");
});

test("never zero tabs: closing the last tab is refused", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  assert.equal(tabs.closeTab(a.id), false, "closing the only tab is a no-op");
  assert.equal(tabs.count, 1);
  assert.equal(a.disposed, false);
  assert.equal(onlyVisible(tabs).id, a.id);
});

test("closing the active tab activates a neighbor and disposes the closed one", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  const b = tabs.newTab(); // b active
  assert.equal(tabs.closeTab(b.id), true);
  assert.equal(tabs.count, 1);
  assert.equal(b.disposed, true, "the closed tab is disposed (its PTYs killed)");
  assert.equal(tabs.activeTabId, a.id, "a neighbor becomes active");
  assert.equal(onlyVisible(tabs).id, a.id);
});

test("closing a background tab leaves the active tab untouched", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  const b = tabs.newTab(); // b active
  const c = tabs.newTab(); // c active
  a.focuses = 0;
  c.focuses = 0;
  assert.equal(tabs.closeTab(a.id), true);
  assert.equal(tabs.activeTabId, c.id, "active is unchanged");
  assert.equal(a.disposed, true);
  assert.equal(c.focuses, 0, "no needless refocus of the active tab");
});

test("next/prev cycle through tabs and wrap around", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  const b = tabs.newTab();
  const c = tabs.newTab(); // c active
  tabs.nextTab();
  assert.equal(tabs.activeTabId, a.id, "next from the last wraps to the first");
  tabs.prevTab();
  assert.equal(tabs.activeTabId, c.id, "prev from the first wraps to the last");
  tabs.prevTab();
  assert.equal(tabs.activeTabId, b.id);
});

test("cycling is a no-op with a single tab", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  a.focuses = 0;
  tabs.nextTab();
  tabs.prevTab();
  assert.equal(tabs.activeTabId, a.id);
  assert.equal(a.focuses, 0, "no switch happened");
});

test("rename rejects blank names, keeps non-blank (trimmed)", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  tabs.renameTab(a.id, "  Backend  ");
  assert.equal(a.name, "Backend");
  tabs.renameTab(a.id, "   ");
  assert.equal(a.name, "Backend", "a blank rename is rejected");
});

test("setColor sets and clears the tab accent", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  tabs.setColor(a.id, "#9ece6a");
  assert.equal(a.color, "#9ece6a");
  tabs.setColor(a.id, null);
  assert.equal(a.color, null);
});

test("onChange fires on add / switch / rename / close and unsubscribes", () => {
  const { tabs } = makeManager();
  let n = 0;
  const off = tabs.onChange(() => n++);
  const a = tabs.newTab(); // +1
  const b = tabs.newTab(); // +1
  tabs.switchTo(a.id); // +1
  tabs.renameTab(a.id, "x"); // +1
  tabs.closeTab(b.id); // +1
  assert.equal(n, 5);
  off();
  tabs.newTab();
  assert.equal(n, 5, "no more notifications after unsubscribe");
});

// ---- phase-3 routing seams (worker B wires the tab-aware router) ----

test("routing: group and pty bind to a workspace and resolve back", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  const b = tabs.newTab();
  tabs.bindGroup("grp-1", a.id);
  tabs.bindPty(42, a.id);
  assert.equal(tabs.workspaceForGroup("grp-1")?.id, a.id);
  assert.equal(tabs.workspaceForPty(42)?.id, a.id);
  assert.equal(tabs.workspaceForGroup("nope"), undefined);
  assert.equal(tabs.workspaceForPty(999), undefined);
  // b has no bindings.
  assert.equal(tabs.workspaceForGroup("grp-1")?.id !== b.id, true);
});

test("routing: closing a tab forgets its group/pty bindings", () => {
  const { tabs } = makeManager();
  const a = tabs.newTab();
  tabs.newTab(); // keep a second tab so a can be closed (never zero tabs)
  tabs.bindGroup("grp-1", a.id);
  tabs.bindPty(42, a.id);
  assert.equal(tabs.closeTab(a.id), true);
  assert.equal(tabs.workspaceForGroup("grp-1"), undefined, "stale group route dropped");
  assert.equal(tabs.workspaceForPty(42), undefined, "stale pty route dropped");
});
