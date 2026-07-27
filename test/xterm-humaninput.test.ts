// #440 B2-R (review round 3): pins the xterm.js fact `Pane.ts`'s human-input
// signal depends on — that `term.onData` fires for data the terminal
// generates ENTIRELY ON ITS OWN, with no key pressed, and is therefore NOT a
// safe "did a human type this" signal.
//
// This is the SAME mechanism as #179 ("copilot boot OSC/DCS color queries
// misread as human input"): per #179's own investigation, GitHub Copilot
// queries the terminal's colors (`ESC]10;?`, `ESC]11;?`, `ESC]4;n;?`) and
// version (`ESC[>q`) at boot, and xterm answers those automatically; that
// answer flows out through `onData` exactly like a keystroke would, because
// `onData` is the terminal's ENTIRE "data destined for the PTY" channel, not
// a human-input-only one — xterm's internal `wasUserInput` flag (see
// `@xterm/xterm/lib/xterm.js`'s `CoreService.triggerDataEvent`) gates only
// scroll-to-bottom and xterm's own internal `_onUserInput` event, never the
// public `onData` fired below it.
//
// WHAT THIS FILE CAN AND CANNOT REPRODUCE (checked empirically, not assumed):
// `@xterm/headless` has no renderer/theme service, so the SPECIFIC color/
// version queries #179 cites (OSC 10/11/4, `ESC[>q`) silently produce no
// reply here regardless of `theme`/terminator options — verified by direct
// probing, not inferred. It also has no `onKey` at all (a DOM-keyboard-event
// feature; not part of the headless API surface), so "onKey never fires for
// an auto-reply" can't be pinned directly either — which is itself the
// property that makes it safe: unreachable without a real `KeyboardEvent`.
// What headless DOES faithfully reproduce, verified below, is Primary/
// Secondary Device Attributes (`CSI c` / `CSI >c`) — a capability query many
// TUIs issue at boot, CLI-agnostic, and — like OSC 10/11/4 — answered by
// xterm with ZERO human input. That's the general form of the same fact
// #179 hit for copilot's specific query shapes: `onData` fires for
// terminal-manufactured replies, not only for typed/pasted human input. The
// copilot-specific OSC 10/11 shapes are covered instead by a live
// hand-validation item (see the PR) run against the real app, where xterm
// DOES have a renderer/theme service to answer from.
import { test } from "node:test";
import assert from "node:assert/strict";
import pkg from "@xterm/headless";
const { Terminal } = pkg;

function write(term: InstanceType<typeof Terminal>, data: string): Promise<void> {
  return new Promise((resolve) => term.write(data, resolve));
}

test("a primary Device Attributes query (CSI c) auto-replies via onData with zero human input", async () => {
  const term = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });
  const dataEvents: string[] = [];
  term.onData((d) => dataEvents.push(d));

  await write(term, "\x1b[c"); // what a TUI sends to ask "what kind of terminal are you"

  assert.equal(dataEvents.length, 1, "xterm answered its own capability query with no key pressed");
  assert.match(dataEvents[0], /^\x1b\[\?\d/); // CSI ? Ps c — a primary DA response
});

test("a secondary Device Attributes query (CSI >c) auto-replies via onData too", async () => {
  const term = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });
  const dataEvents: string[] = [];
  term.onData((d) => dataEvents.push(d));

  await write(term, "\x1b[>c");

  assert.equal(dataEvents.length, 1);
  assert.match(dataEvents[0], /^\x1b\[>\d/); // CSI > Ps ; Ps ; Ps c
});

test("plain, non-query output produces NO onData reply — a query specifically is what triggers the auto-answer", async () => {
  const term = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });
  const dataEvents: string[] = [];
  term.onData((d) => dataEvents.push(d));

  await write(term, "just some ordinary program output, no query in it\r\n");

  assert.equal(dataEvents.length, 0);
});

test("@xterm/headless has no onKey at all — confirms it's a real-DOM-keyboard-event feature, unreachable by anything the terminal generates on its own", () => {
  const term = new Terminal({ cols: 80, rows: 24 });
  // Not a defect in headless — the opposite: this is WHY `Pane.ts` marking
  // `firstInputMs` from `onKey` is safe. If headless (no DOM, no real
  // KeyboardEvent) could still fire it, onKey wouldn't be the human-only
  // signal the fix depends on.
  assert.equal(typeof (term as unknown as { onKey?: unknown }).onKey, "undefined");
});
