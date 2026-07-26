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
// fix #430 -- the first test below pins that (still red on a stock v6 bump).
//
// The only lever that measurably helps is xterm.js#5234's `reflowCursorLine`
// option (added in 6.0.0, off by default "because shells usually handle this
// themselves" -- which conpty's resize-quirk explicitly does not): it makes
// xterm reflow the cursor's own line like everything else. pane.ts sets it
// alongside windowsPty, gated to the conpty branch. It is NOT a confirmed
// complete fix upstream (its own author: "results in the cursor being in a
// different place than where it started ... maybe this behavior is
// expected?") -- see the PR for what a human should still watch for by eye.
//
// The VT stream below is hand-constructed from the upstream repro shape
// (wrapped output, a live width change, continued output at the old cursor)
// per xterm.js#5319 / microsoft/terminal#18725's minimal repro description
// -- not a captured real PTY trace (no safe repro was constructed for a live
// capture; see the PR description).
import { test } from "node:test";
import assert from "node:assert/strict";
import pkg from "@xterm/headless";
import type { Terminal as TerminalType } from "@xterm/headless";
const { Terminal } = pkg;

const MARKER = "EchoMark";

function write(term: TerminalType, data: string): Promise<void> {
  return new Promise((resolve) => term.write(data, resolve));
}

/** Replays: a wrapped "prompt line" streamed with no trailing newline (so the
 *  cursor stays ON the wrapped line -- the row xterm's reflow refuses to
 *  touch by default), a layout resize (the divider-drag/maximize trigger
 *  traced through grid.ts/pane.ts -- never a PTY-only resize), then
 *  continued output at the old cursor position -- standing in for whatever
 *  conpty's VT passthrough sends assuming the buffer already reflowed. */
async function replay(reflowCursorLine: boolean) {
  const term = new Terminal({
    cols: 40,
    rows: 24,
    scrollback: 1000,
    allowProposedApi: true,
    reflowCursorLine,
  });
  // Matches pane.ts's start(): the sideloaded conpty build pty.rs reports.
  term.options.windowsPty = { backend: "conpty", buildNumber: 22621 };

  const content = "PROMPT> " + "w".repeat(120); // 128 cols, wraps at width 40
  await write(term, content);

  term.resize(80, 24);
  await write(term, MARKER);

  const buf = term.buffer.active;
  const rows: string[] = [];
  for (let y = 0; y < buf.length; y++) {
    rows.push(buf.getLine(y)?.translateToString(true) ?? "");
  }
  const markerRow = rows.findIndex((r) => r.includes(MARKER));
  // Geometrically correct row once the 128-char line is laid out at the NEW
  // width (80): floor(128 / 80) = row 1. Any other row means the echo landed
  // on stale, pre-resize geometry -- the bug.
  const expectedRow = Math.floor(content.length / 80);
  return { markerRow, expectedRow, rows };
}

test("#430: pane.ts's conpty windowsPty + reflowCursorLine config reflows the cursor's wrapped line on resize", async () => {
  const { markerRow, expectedRow, rows } = await replay(true);
  assert.equal(
    markerRow,
    expectedRow,
    `echo landed on row ${markerRow}, expected the reflowed row ${expectedRow}. Buffer:\n${rows.join("\n")}`
  );
});

test("#430: without reflowCursorLine (a stock @xterm/xterm 6.0.0 bump), the bug still reproduces", async () => {
  const { markerRow, expectedRow } = await replay(false);
  assert.notEqual(
    markerRow,
    expectedRow,
    "expected the echo on a stale row (the bug) when reflowCursorLine is left at its xterm default"
  );
});

test("#430 KNOWN LIMITATION: reflowCursorLine does not fix a NARROWING resize (widening only, verified above)", async () => {
  // Mirror of replay() above but shrinking cols instead of growing them --
  // the direction xterm.js#5234's own author flagged as unresolved ("results
  // in the cursor being in a different place than where it started"). This
  // pins the CURRENT (imperfect) behavior, not the desired one: with the
  // reflow forced on, the echo doesn't just land on a stale row -- it gets
  // split across two rows with a stray run of blank padding, which is worse
  // than merely "wrong row". If a future @xterm/xterm release fixes
  // narrowing too, this test starts failing -- that's the point: replace
  // this assertion with an equality check like the widening test above
  // instead of loosening it, and drop the mention of this case from the PR
  // template. Until then, this is what to still watch for by eye per-pane
  // (docs/design or the PR's manual-check list).
  const term = new Terminal({
    cols: 80,
    rows: 24,
    scrollback: 1000,
    allowProposedApi: true,
    reflowCursorLine: true,
  });
  term.options.windowsPty = { backend: "conpty", buildNumber: 22621 };

  const content = "PROMPT> " + "w".repeat(120);
  await write(term, content);
  term.resize(40, 24); // narrower, not wider
  await write(term, MARKER);

  const buf = term.buffer.active;
  const rows: string[] = [];
  for (let y = 0; y < buf.length; y++) {
    rows.push(buf.getLine(y)?.translateToString(true) ?? "");
  }
  const markerRow = rows.findIndex((r) => r.includes(MARKER));
  assert.equal(
    markerRow,
    -1,
    `expected the known-limitation split (marker not intact on any single row); ` +
      `it landed intact on row ${markerRow} -- narrowing may have been fixed upstream, see the comment above.\n${rows.join("\n")}`
  );
});
