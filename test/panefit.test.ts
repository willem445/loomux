// The load-bearing no-resize invariant (#63, CLAUDE.md constraint 1), tested at
// its pure core: a hidden pane (display:none — an inactive project tab, or a
// pane behind a maximized sibling) reports zero client width and must issue NO
// PTY resize, because resizing ConPTY repaints the whole screen into scrollback
// on the Win10 inbox conhost. This is the regression the plan calls for,
// mirroring the maximize precedent (styles.css `.has-maximized`). Run `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { shouldResizePty } from "../src/panefit.ts";

test("a hidden (zero-width) pane never resizes, even when the fitted size differs", () => {
  // This is exactly the tab-switch / maximize case: the pane is display:none, so
  // clientWidth is 0. Even though a stale fit says the size "changed", no resize
  // may go to the PTY.
  assert.equal(
    shouldResizePty({ clientWidth: 0, size: "120x40", sentSize: "80x24", ptyId: 7 }),
    false,
    "zero width must suppress the resize regardless of size delta"
  );
});

test("a visible pane whose size actually changed does resize", () => {
  assert.equal(
    shouldResizePty({ clientWidth: 800, size: "120x40", sentSize: "80x24", ptyId: 7 }),
    true
  );
});

test("a same-size fit is skipped (ConPTY resize is never free)", () => {
  assert.equal(
    shouldResizePty({ clientWidth: 800, size: "80x24", sentSize: "80x24", ptyId: 7 }),
    false
  );
});

test("a pane with no PTY yet never resizes", () => {
  assert.equal(
    shouldResizePty({ clientWidth: 800, size: "80x24", sentSize: "", ptyId: null }),
    false
  );
});
