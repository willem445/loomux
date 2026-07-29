// The load-bearing no-resize invariant (#63, CLAUDE.md constraint 1), tested at
// its pure core: a hidden pane (display:none — an inactive project tab, or a
// pane behind a maximized sibling) reports zero client width and must issue NO
// PTY resize, because resizing ConPTY repaints the whole screen into scrollback
// on the Win10 inbox conhost. This is the regression the plan calls for,
// mirroring the maximize precedent (styles.css `.has-maximized`). Run `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { shouldResizePty } from "../src/panefit.ts";

// held/pending default to the "not dragging, nothing in flight" case so each
// test below only names the field it's actually exercising.
const base = { held: false, pending: null as string | null };

test("a hidden (zero-width) pane never resizes, even when the fitted size differs", () => {
  // This is exactly the tab-switch / maximize case: the pane is display:none, so
  // clientWidth is 0. Even though a stale fit says the size "changed", no resize
  // may go to the PTY.
  assert.equal(
    shouldResizePty({ ...base, clientWidth: 0, size: "120x40", sentSize: "80x24", ptyId: 7 }),
    false,
    "zero width must suppress the resize regardless of size delta"
  );
});

test("a visible pane whose size actually changed does resize", () => {
  assert.equal(
    shouldResizePty({ ...base, clientWidth: 800, size: "120x40", sentSize: "80x24", ptyId: 7 }),
    true
  );
});

test("a same-size fit is skipped (ConPTY resize is never free)", () => {
  assert.equal(
    shouldResizePty({ ...base, clientWidth: 800, size: "80x24", sentSize: "80x24", ptyId: 7 }),
    false
  );
});

test("a pane with no PTY yet never resizes", () => {
  assert.equal(
    shouldResizePty({ ...base, clientWidth: 800, size: "80x24", sentSize: "", ptyId: null }),
    false
  );
});

// #432 item 1: resize-storm coalescing. A divider drag holds every tick's
// resize back, however many fit ticks land during the drag, so what would
// otherwise be one ResizePseudoConsole per animation frame collapses to
// nothing until the drag's own `end` handler flushes (see pane.test.ts /
// hand-verification for the DOM wiring — this is the pure decision alone).
test("a held (in-drag) tick never resizes, even when the size changed", () => {
  assert.equal(
    shouldResizePty({ clientWidth: 800, size: "120x40", sentSize: "80x24", ptyId: 7, held: true, pending: null }),
    false
  );
});

test("releasing the hold (held: false) resizes normally once the drag settles", () => {
  assert.equal(
    shouldResizePty({ clientWidth: 800, size: "120x40", sentSize: "80x24", ptyId: 7, held: false, pending: null }),
    true
  );
});

// #432 item 3: sentSize only latches on a successful resize now, so a second
// fit tick can land while the first call's IPC round-trip is still
// outstanding. Without the `pending` guard that would fire an identical
// duplicate call every tick until the first one resolves.
test("a size identical to one already in flight is not resent", () => {
  assert.equal(
    shouldResizePty({ clientWidth: 800, size: "120x40", sentSize: "80x24", ptyId: 7, held: false, pending: "120x40" }),
    false
  );
});

test("a size that differs from BOTH sentSize and the in-flight one still resizes", () => {
  // The drag kept moving: a third size arrived before the first call (still
  // in flight) resolved. That's a real, distinct geometry change and must
  // still go out — `pending` only dedups an exact repeat of itself.
  assert.equal(
    shouldResizePty({ clientWidth: 800, size: "130x45", sentSize: "80x24", ptyId: 7, held: false, pending: "120x40" }),
    true
  );
});
