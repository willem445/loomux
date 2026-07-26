// Regression pin for #430: loomux sideloads conpty 1.22 (VT passthrough +
// PSEUDOCONSOLE_RESIZE_QUIRK), which never repaints on resize and trusts the
// terminal to reflow itself. xterm.js's own reflow (Buffer.ts's
// _reflowLarger/_reflowSmaller) has always refused to touch the row the
// cursor sits on -- intentionally, on the assumption the shell repaints its
// own prompt line -- which is exactly the assumption conpty's resize-quirk
// mode breaks. PSReadLine then repaints its input line at cursor coordinates
// conpty reports assuming a fully-reflowed buffer, landing inside stale,
// already-rendered output.
//
// The investigation that scoped this PR (see #430's investigation comment)
// cited xterm.js#5321 ("Reflow on resize using similar logic to conpty",
// released in @xterm/xterm 6.0.0) as an upstream fix already released. That
// turned out not to hold up: #5321 was reverted wholesale by xterm.js#5358
// after it bricked buffers in VS Code, which reopened xterm.js#5319 --
// loomux's exact signature bug. #5319 is closed again, but as "track this in
// microsoft/vscode#224488 for now" (maintainer comment), not as fixed; that
// VS Code issue is itself closed with the same conpty.dll sideload disabled
// in Insiders over open resize-duplication reports against the very conpty
// build loomux ships (1.22.250204002). Bumping @xterm/xterm alone does NOT
// fix #430 -- pinned below (still red on a stock v6 bump).
//
// The lever that measurably helps is xterm.js#5234's `reflowCursorLine`
// option (added in 6.0.0, off by default "because shells usually handle this
// themselves" -- which conpty's resize-quirk explicitly does not). pane.ts
// sets it alongside windowsPty, gated to the conpty branch.
//
// PRECISE characterization (measured, not assumed -- see #430 follow-up
// issue for the tracker this feeds): reflowCursorLine fixes the cursor's
// ROW in BOTH resize directions, and fixes the COLUMN in NEITHER direction,
// by design: xterm.js#5522 (merged, milestone 7.0.0, not yet released)
// documents "this will not move the cursor position, only the line
// contents" against xterm.js#5295 ("Cursor is incorrectly positioned on
// reflow of cursor line"). So the row lands right; the column does not, on
// every resize, regardless of direction. Whether a subsequent write visibly
// SPLITS across a row boundary is incidental -- it depends on how far the
// (always wrong) column sits from the new row's width, not on a real
// widen-vs-narrow asymmetry in the underlying defect.
//
// There is a real, if partial, upside worth pinning too: WITHOUT
// reflowCursorLine, a narrowing resize doesn't reflow the cursor's line at
// all, and content beyond the old (wider) row boundary becomes unreachable
// -- verified below as content loss (72 of 120 characters survive). WITH
// the option, the same resize preserves all of it (see the content-
// preservation test), even though the cursor still ends up in the wrong
// column.
//
// The VT stream below is hand-constructed from the upstream repro shape
// (wrapped output, a live width change) per xterm.js#5319 / microsoft/
// terminal#18725's minimal repro description -- not a captured real PTY
// trace (no safe repro was constructed for a live capture; see the PR
// description).
import { test } from "node:test";
import assert from "node:assert/strict";
import pkg from "@xterm/headless";
const { Terminal } = pkg;

const CONTENT = "PROMPT> " + "w".repeat(120); // one long wrapped "prompt line", 128 cols

function write(term: InstanceType<typeof Terminal>, data: string): Promise<void> {
  return new Promise((resolve) => term.write(data, resolve));
}

/** Streams CONTENT with no trailing newline (so the cursor stays ON the
 *  wrapped line -- the row xterm's reflow refuses to touch by default),
 *  then a layout resize (the divider-drag/maximize trigger traced through
 *  grid.ts/pane.ts -- never a PTY-only resize). Returns the cursor's actual
 *  position after the resize plus what a fully-correct reflow would have
 *  produced, and a count of how many of the 120 'w' characters are still
 *  present anywhere in the buffer (content-preservation check). */
async function replay(cols0: number, cols1: number, reflowCursorLine: boolean) {
  const term = new Terminal({
    cols: cols0,
    rows: 24,
    scrollback: 1000,
    allowProposedApi: true,
    reflowCursorLine,
  });
  // Matches pane.ts's start(): the sideloaded conpty build pty.rs reports.
  term.options.windowsPty = { backend: "conpty", buildNumber: 22621 };

  await write(term, CONTENT);
  term.resize(cols1, 24);

  const buf = term.buffer.active;
  const rows: string[] = [];
  let wCount = 0;
  for (let y = 0; y < buf.length; y++) {
    const s = buf.getLine(y)?.translateToString(true) ?? "";
    rows.push(s);
    for (const ch of s) if (ch === "w") wCount++;
  }
  const expectedRow = Math.floor(CONTENT.length / cols1);
  const expectedCol = CONTENT.length - expectedRow * cols1;
  return { cursorY: buf.cursorY, cursorX: buf.cursorX, expectedRow, expectedCol, wCount, rows };
}

test("#430: reflowCursorLine corrects the cursor ROW on a widening resize (stock v6 bump alone does not)", async () => {
  const withFix = await replay(40, 80, true);
  assert.equal(
    withFix.cursorY,
    withFix.expectedRow,
    `cursor row ${withFix.cursorY}, expected the reflowed row ${withFix.expectedRow}.\n${withFix.rows.join("\n")}`
  );

  const withoutFix = await replay(40, 80, false);
  assert.notEqual(
    withoutFix.cursorY,
    withoutFix.expectedRow,
    "expected the row bug to reproduce (stale row) when reflowCursorLine is left at its xterm default"
  );
});

test("#430: reflowCursorLine corrects the cursor ROW on a narrowing resize too (row fix is not widen-only)", async () => {
  const withFix = await replay(80, 40, true);
  assert.equal(
    withFix.cursorY,
    withFix.expectedRow,
    `cursor row ${withFix.cursorY}, expected the reflowed row ${withFix.expectedRow}.\n${withFix.rows.join("\n")}`
  );

  const withoutFix = await replay(80, 40, false);
  assert.notEqual(
    withoutFix.cursorY,
    withoutFix.expectedRow,
    "expected the row bug to reproduce (stale row) when reflowCursorLine is left at its xterm default"
  );
});

test("#430 KNOWN LIMITATION: reflowCursorLine never corrects the cursor COLUMN, by design (xterm.js#5522)", async () => {
  // Row-only-honest: the two tests above prove the ROW lands right. This
  // proves the COLUMN does not, in EITHER direction, with the option on --
  // xterm.js#5522 documents this is intentional ("will not move the cursor
  // position, only the line contents"), not a bug pending a fix. Don't read
  // the widening tests above as "the cursor ends up right" -- only the row
  // does.
  for (const [cols0, cols1] of [
    [40, 80],
    [80, 40],
  ] as const) {
    const r = await replay(cols0, cols1, true);
    assert.notEqual(
      r.cursorX,
      r.expectedCol,
      `cols ${cols0}->${cols1}: expected the column to still be wrong (cursorX=${r.cursorX}, ` +
        `a correct reflow would put it at ${r.expectedCol}) -- if this now passes, xterm.js has ` +
        `shipped a real cursor-column fix; update this test and doc/design/xterm-resize-reflow.md.`
    );
  }
});

test("#430: reflowCursorLine prevents content loss on a narrowing resize (a real win, even though the column stays wrong)", async () => {
  const withoutFix = await replay(80, 40, false);
  assert.ok(
    withoutFix.wCount < 120,
    `sanity check: content loss should reproduce without the option (saw ${withoutFix.wCount}/120)`
  );

  const withFix = await replay(80, 40, true);
  assert.equal(
    withFix.wCount,
    120,
    `expected all content to survive the reflow with reflowCursorLine on (saw ${withFix.wCount}/120)`
  );
});
