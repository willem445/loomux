// #720, self-review finding: the render throttle made `markFirstInput` flush
// held output, and the ORDER of that flush relative to `humanOrigin.mark()` is
// load-bearing. This file pins the upstream xterm behaviour the ordering rests
// on, so the constraint is enforced rather than merely asserted in a comment.
//
// The hazard. `humanorigin.ts`'s entire correctness argument is that data the
// terminal manufactures for itself (a DA/OSC auto-reply) "arrives while its own
// `term.write()` is being parsed — always a different turn — and therefore
// always reads false". That holds only while every `term.write()` is parsed on
// a LATER task than the keystroke. `WriteBuffer.write` breaks exactly that when
// a write lands on an empty buffer right after user input — its `_didUserInput`
// fast path calls `_innerWrite()` inline to cut echo latency
// (@xterm/xterm/src/common/input/WriteBuffer.ts). So a flush called from inside
// `markFirstInput` AFTER the mark would parse synchronously, inside the marked
// turn, and hand the backend's keystroke clock a program-generated reply as a
// human keystroke — the #179/#518 failure, re-created by a perf change.
//
// pane.ts's fix is to flush BEFORE marking. These two tests are the A/B that
// shows the fast path is real and is armed by exactly the signal xterm's own
// `onKey` accompanies; if xterm ever stopped parsing synchronously there, the
// first test goes red and the ordering constraint can be revisited on evidence
// instead of being carried forever on a comment. Same "pin the upstream fact
// the fix leans on" shape as xterm-write-queue.test.ts and xterm-reflow.test.ts.
import { test } from "node:test";
import assert from "node:assert/strict";
import pkg from "@xterm/headless";
const { Terminal } = pkg;

/** Primary DA (`CSI c`) — a query the terminal answers ITSELF, with no process
 *  and no human involved. Exactly the class of self-generated data #440 B2-R
 *  and #518 exist to keep out of the human-input signal. */
const DA_QUERY = "\x1b[c";

test("#720: a write right after user input is parsed SYNCHRONOUSLY, so its auto-reply lands in the same turn", () => {
  const term = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });
  let phase = "before-write";
  let replyPhase: string | null = null;
  term.onData(() => {
    replyPhase ??= phase;
  });

  // Arm xterm's `_didUserInput` fast path exactly as a keystroke does: this is
  // the same `triggerDataEvent(data, /* wasUserInput */ true)` that xterm's own
  // key handling performs, i.e. the state a pane is in the instant `onKey` —
  // and therefore `markFirstInput` — runs.
  term.input("a", true);
  replyPhase = null; // that keystroke's own onData is not what we're measuring

  phase = "during-write";
  term.write(DA_QUERY);
  phase = "after-write";

  assert.equal(
    replyPhase,
    "during-write",
    "xterm answered the query INSIDE term.write(). Any mark opened before this write would " +
      "therefore be open while the terminal talks to itself — which is why pane.ts's " +
      "markFirstInput flushes held output BEFORE humanOrigin.mark()."
  );
});

test("#720 contrast: with no preceding user input the same write is parsed on a LATER task", async () => {
  // The other arm, and the reason the ordering is a real constraint rather than
  // a general truth: without the fast path armed, xterm defers the parse, which
  // is the "always a different turn" that humanorigin.ts's argument assumes. The
  // throttle's flush is the one thing that can move a write INTO the armed case.
  const term = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });
  let phase = "before-write";
  let replyPhase: string | null = null;
  term.onData(() => {
    replyPhase ??= phase;
  });

  phase = "during-write";
  term.write(DA_QUERY);
  phase = "after-write";

  assert.equal(replyPhase, null, "nothing should have been emitted synchronously");
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
  assert.equal(replyPhase, "after-write", "the auto-reply arrives on a later task");
});
