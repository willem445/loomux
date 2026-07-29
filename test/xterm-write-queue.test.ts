// #432 item 2: pane.ts's doFit() defers the actual geometry change (fit.fit()
// + the resize decision, in doResize()) behind `term.write("", () =>
// this.doResize())` instead of calling fit.fit() directly. The reasoning:
// fit.fit() resizes xterm's buffer SYNCHRONOUSLY, but term.write() is parsed
// ASYNCHRONOUSLY (xterm's own internal WriteBuffer, chunked so a huge paste
// doesn't block the main thread) -- so a resize that ran the instant a
// debounced fit tick fired could land while output the PTY already sent
// (under the OLD geometry) was still sitting unparsed in that queue, and get
// interpreted under the NEW one once it finally got processed.
//
// pane.ts itself can't be unit-tested here (it's DOM/Tauri-IPC-bound --
// CLAUDE.md's DOM-free-pure-module convention, mirroring test/xterm-
// reflow.test.ts's own split for the same reason). What CAN be pinned
// DOM-free, with @xterm/headless, is the actual invariant the fix leans on:
// that term.write()'s own callbacks fire in the SAME order the writes were
// queued in, so an empty write queued after a real one is guaranteed to run
// its callback only once that real write has been fully parsed into the
// buffer -- regardless of how long xterm's internal chunking takes. If
// xterm.js ever stopped guaranteeing that (e.g. reordered callbacks for an
// empty write as a fast-path optimization), this fix would silently stop
// working, and this test would catch it.
import { test } from "node:test";
import assert from "node:assert/strict";
import pkg from "@xterm/headless";
const { Terminal } = pkg;

// Long enough that xterm's WriteBuffer has to chunk it across more than one
// internal batch rather than finishing inline within the write() call --
// the exact "already-queued, not-yet-parsed" state a resize must never race.
const BIG_WRITE = "x".repeat(50_000);

test("#432: an empty term.write(\"\", cb) queued after a real write only fires once that write has been parsed", async () => {
  const term = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });

  const order: string[] = [];
  // Fire-and-forget, exactly like attachOutput's `this.term.write(bytes)` in
  // pane.ts -- nothing awaits this before the next write is queued, so at
  // the moment the empty write below is queued, this one may still be
  // in-flight inside xterm's own buffer.
  term.write(BIG_WRITE, () => order.push("real-write-parsed"));

  await new Promise<void>((resolve) => {
    // This is doFit()'s own call, standing in for the debounced fit tick
    // that would otherwise call fit.fit() directly and race the write above.
    term.write("", () => {
      order.push("deferred-resize-ran");
      resolve();
    });
  });

  assert.deepEqual(
    order,
    ["real-write-parsed", "deferred-resize-ran"],
    "the deferred resize callback must never run before output queued ahead of it has been parsed -- " +
      "otherwise doResize()'s fit.fit() can observe a buffer that hasn't caught up to what the PTY already sent"
  );
  // And the buffer itself must actually reflect the write by the time the
  // deferred callback runs -- not just that its OWN callback fired (belt and
  // suspenders: this is the property doResize()'s fit.fit() actually needs).
  assert.equal(term.buffer.active.cursorX + term.buffer.active.cursorY * term.cols > 0, true);
});
